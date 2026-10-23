#!/usr/bin/env bash
# Conformance lanes for the ruby target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane builds and installs
# one generated gem and runs one consumer against it, which must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
. "$ROOT/scripts/toolchains.sh"
use_ruby || require_tools ruby ruby3.2
require_tools ruby ruby gem

# A private gem home per Ruby version, holding the generated gems and, when
# the Ruby found has no `ffi` gem (which the bindings depend on), a pinned
# one installed here rather than into the Ruby's own gem home.
FFI_GEM_VERSION=1.17.2
RUBY_GEM_HOME="$OUT/ruby-gems/$(ruby -e 'print RUBY_VERSION')"
export RUBY_GEM_HOME
if ! GEM_PATH="$RUBY_GEM_HOME:$(gem env gempath)" ruby -e "require 'ffi'" >/dev/null 2>&1; then
    echo "--- installing ffi $FFI_GEM_VERSION into $RUBY_GEM_HOME"
    gem install --silent --no-document --install-dir "$RUBY_GEM_HOME" ffi -v "$FFI_GEM_VERSION" || exit 1
fi

# Build the sample's generated gem with `gem build`, install it into the
# private gem home, and run the consumer against the installed gem. The
# library is selected through the generated loader's {PREFIX}_LIBRARY
# override.
rb_consumer() {
    local sample="$1" script="$2"
    local gemdir="$GENROOT/$sample/ruby" home="$RUBY_GEM_HOME"
    local spec gem
    spec=$(cd "$gemdir" && ls ./*.gemspec) || return 1
    gem="$OUT/ruby-${sample}.gem"
    (cd "$gemdir" && gem build --silent "$spec" -o "$gem") || return 1
    gem install --silent --local --ignore-dependencies --no-document \
        --install-dir "$home" "$gem" || return 1
    env GEM_PATH="$home:$(gem env gempath)" \
        "$(library_env "$sample")=$(sample_lib "$sample")" \
        ruby -w "$ROOT/conformance/ruby/$script"
}

ruby_calculator() { rb_consumer calculator calculator.rb; }
ruby_codec() { rb_consumer codec codec.rb; }
ruby_kvstore() { rb_consumer kvstore kvstore.rb; }

lane ruby-calculator ruby_calculator
lane ruby-codec ruby_codec
lane ruby-kvstore ruby_kvstore

finish_lanes
