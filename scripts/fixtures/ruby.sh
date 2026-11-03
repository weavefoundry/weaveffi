#!/usr/bin/env bash
# Syntax-check every generated Ruby file with warnings enabled (any warning
# fails), build the gem from its generated gemspec, and then load the
# bindings for real: a stub library built from the C header generated
# alongside (every function returns its zero value, and the contract
# functions are the header's own, from its DEFINE_CONTRACT macro) lets
# `require` run the ABI and contract checks, attach every function, and
# evaluate every class, layout, and vtable, again with warnings enabled.
#
# The bindings need the `ffi` gem. When the Ruby found has none, a pinned
# version is installed into the cargo target directory (never into the
# Ruby's own gem home).
set -euo pipefail
. "$(dirname "$0")/lib.sh"
. "$(dirname "$0")/../toolchains.sh"
use_ruby || missing "Ruby 3.2 or newer not found"
require gem cc
dir=$(cd "$1" && pwd)
status=0

FFI_GEM_VERSION=1.17.2
if ! ruby -e "require 'ffi'" >/dev/null 2>&1; then
    root=$(cd "$(dirname "$0")/../.." && pwd)
    target=$(cd "$root" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
        | tr ',' '\n' | grep '"target_directory"' | head -1 \
        | sed 's/.*"target_directory":"//; s/"$//')
    home="${target:-$root/target}/ruby-gems/$(ruby -e 'print RUBY_VERSION')"
    if ! GEM_PATH="$home" ruby -e "require 'ffi'" >/dev/null 2>&1; then
        gem install --silent --no-document --install-dir "$home" ffi -v "$FFI_GEM_VERSION" >&2 \
            || missing "the ffi gem could not be installed"
    fi
    export GEM_PATH="$home${GEM_PATH:+:$GEM_PATH}"
fi

while IFS= read -r file; do
    if ! out=$(ruby -wc "$file" 2>&1 >/dev/null) || [ -n "$out" ]; then
        echo "$file:" >&2
        echo "$out" >&2
        status=1
    fi
done < <(find "$dir/ruby" -name '*.rb' -o -name '*.gemspec')
spec=$(cd "$dir/ruby" && ls ./*.gemspec)
(cd "$dir/ruby" && gem build --silent "$spec" -o "$dir/ruby-fixture.gem") || status=1

lib=$(find "$dir/ruby/lib" -maxdepth 1 -name '*.rb' | head -1)
feature=$(basename "$lib" .rb)
header=$(find "$dir/c" -name '*.h' ! -name '*_buffer.h' | head -1)
env_var=$(sed -n "s/.*ENV.fetch('\([A-Z0-9_]*\)'.*/\1/p" "$dir/ruby/lib/$feature/runtime.rb")
case "$(uname -s)" in
    Darwin) stub="$dir/libruby_stub.dylib" ;;
    *) stub="$dir/libruby_stub.so" ;;
esac
ruby - "$header" > "$dir/ruby_stub.c" <<'RUBY'
header = ARGV.fetch(0)
src = File.read(header)
api = src[/#ifndef (\w+_API)/, 1]
puts %(#include "#{File.basename(header)}")
# Every declaration that ends its line (contract functions end with a
# comment, and the header's macro defines them).
src.scan(/^#{api} (.+?)\b(\w+)\((.*)\);$/) do |ret, name, args|
  ret = ret.strip
  body =
    if name.end_with?('_abi_version') then "return #{src[/#define \w+_ABI_VERSION (\d+)u/, 1]};"
    elsif ret == 'void' then ''
    else "static #{ret} zero; return zero;"
    end
  puts "#{ret} #{name}(#{args}) { #{body} }"
end
puts src[/#define (\w+_DEFINE_CONTRACT)/, 1]
RUBY
cc -shared -fPIC -w -I "$dir/c" -o "$stub" "$dir/ruby_stub.c" || status=1
if ! out=$(env "$env_var=$stub" ruby -w -I "$dir/ruby/lib" -e "require '$feature'" 2>&1) || [ -n "$out" ]; then
    echo "loading $feature against the stub library:" >&2
    echo "$out" >&2
    status=1
fi
exit $status
