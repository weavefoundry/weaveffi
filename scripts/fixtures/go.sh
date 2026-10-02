#!/usr/bin/env bash
# Check the generated Go module: gofmt-clean, `go vet`, and `go build` with
# C warnings as errors. No producer exists for a fixture, so the module links
# against an empty stand-in library with unresolved symbols allowed.
set -euo pipefail
dir=$1
mod="$dir/go"
header=$(ls "$mod"/*.h | head -1)
library=$(basename "$header" .h)

stub="$dir/go-stub"
mkdir -p "$stub"
echo 'void go_fixture_stub(void) {}' > "$stub/stub.c"
case "$(uname -s)" in
    Darwin)
        cc -dynamiclib -o "$stub/lib$library.dylib" "$stub/stub.c"
        undefined="-Wl,-undefined,dynamic_lookup"
        ;;
    *)
        cc -shared -fPIC -o "$stub/lib$library.so" "$stub/stub.c"
        undefined="-Wl,--unresolved-symbols=ignore-all"
        ;;
esac

unformatted=$(gofmt -l "$mod")
if [ -n "$unformatted" ]; then
    echo "not gofmt-clean: $unformatted" >&2
    exit 1
fi

cd "$mod"
export GOFLAGS=-mod=mod GOPROXY=off GOSUMDB=off
export CGO_CFLAGS="-Wall -Werror"
export CGO_LDFLAGS="-L$stub $undefined"
go vet ./...
go build ./...
