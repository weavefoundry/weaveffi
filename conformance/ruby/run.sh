#!/usr/bin/env bash
# Conformance lanes for the ruby target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane builds and installs
# one generated gem and runs one consumer against it, which must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
require_tools ruby ruby gem

# Build the sample's generated gem with `gem build`, install it into a
# private gem home, and run the consumer against the installed gem. The
# library is selected through the generated loader's {PREFIX}_LIBRARY
# override.
rb_consumer() {
    local sample="$1" script="$2"
    local gemdir="$GENROOT/$sample/ruby" home="$OUT/ruby-gems"
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
