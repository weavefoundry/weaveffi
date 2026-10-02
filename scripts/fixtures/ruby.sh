#!/usr/bin/env bash
# Syntax-check every generated Ruby file with warnings enabled (any warning
# fails), then build the gem from its generated gemspec.
set -euo pipefail
dir=$(cd "$1" && pwd)
status=0
while IFS= read -r file; do
    if ! out=$(ruby -wc "$file" 2>&1 >/dev/null) || [ -n "$out" ]; then
        echo "$file:" >&2
        echo "$out" >&2
        status=1
    fi
done < <(find "$dir/ruby" -name '*.rb' -o -name '*.gemspec')
spec=$(cd "$dir/ruby" && ls ./*.gemspec)
(cd "$dir/ruby" && gem build --silent "$spec" -o "$dir/ruby-fixture.gem") || status=1
exit $status
