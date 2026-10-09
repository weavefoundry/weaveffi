#!/usr/bin/env bash
# Syntax-check every generated Ruby file with warnings enabled (any warning
# fails), build the gem from its generated gemspec, and then load the
# bindings for real: a stub library built from the C header generated
# alongside (every function returns zero; the contract tables are the
# header's own) lets `require` run the ABI and contract checks, attach every
# function, and evaluate every class, layout, and vtable, again with
# warnings enabled.
set -euo pipefail
. "$(dirname "$0")/lib.sh"
. "$(dirname "$0")/../toolchains.sh"
use_ruby || missing "Ruby 3.2 or newer not found"
require gem cc
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
src.scan(/^#{api} (.+?)\b(\w+)\((.*)\);$/) do |ret, name, args|
  ret = ret.strip
  body =
    if name.end_with?('_contract')
      entry = ret.sub('const ', '').delete('*').strip
      "static const #{entry} e[] = #{name.upcase}; *out_len = sizeof e / sizeof e[0]; return e;"
    elsif name.end_with?('_abi_version') then "return #{src[/#define \w+_ABI_VERSION (\d+)u/, 1]};"
    elsif ret == 'void' then ''
    else 'return 0;'
    end
  puts "#{ret} #{name}(#{args}) { #{body} }"
end
RUBY
cc -shared -fPIC -w -I "$dir/c" -o "$stub" "$dir/ruby_stub.c" || status=1
if ! out=$(env "$env_var=$stub" ruby -w -I "$dir/ruby/lib" -e "require '$feature'" 2>&1) || [ -n "$out" ]; then
    echo "loading $feature against the stub library:" >&2
    echo "$out" >&2
    status=1
fi
exit $status
