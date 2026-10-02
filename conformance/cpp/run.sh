#!/usr/bin/env bash
# Conformance lanes for the cpp target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane compiles and runs one
# consumer against one sample and must exit 0.
#
# Each lane builds a throwaway CMake project that adds the generated `cpp/`
# directory as-is with add_subdirectory and links its `{library}::cpp`
# INTERFACE target. The only input besides the generated tree is the
# `{PREFIX}_LIBRARY` environment variable naming the sample's cdylib. The
# consumer compiles with warnings as errors under AddressSanitizer and
# UndefinedBehaviorSanitizer.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"

# cpp_lane <sample> <consumer source stem>
cpp_lane() {
    local sample=$1 stem=$2
    local lib=${sample//-/_}
    local proj="$OUT/cpp/$sample"
    rm -rf "$proj"
    mkdir -p "$proj"
    cat > "$proj/CMakeLists.txt" <<EOF
cmake_minimum_required(VERSION 3.14)
project(cpp_conformance_$lib CXX)
add_subdirectory("$GENROOT/$sample/cpp" generated)
add_executable(consumer "$ROOT/conformance/cpp/$stem.cpp")
target_include_directories(consumer PRIVATE "$ROOT/conformance/cpp")
target_link_libraries(consumer PRIVATE $lib::cpp)
target_compile_options(consumer PRIVATE -Wall -Wextra -Werror -fsanitize=address,undefined -fno-omit-frame-pointer)
target_link_options(consumer PRIVATE -fsanitize=address,undefined)
EOF
    export "$(library_env "$sample")=$(sample_lib "$sample")"
    cmake -S "$proj" -B "$proj/build" -DCMAKE_BUILD_TYPE=Debug \
        -DCMAKE_CXX_COMPILER="${CXX:-clang++}" >/dev/null \
        && cmake --build "$proj/build" >/dev/null \
        && "$proj/build/consumer"
}

cpp_calculator() { cpp_lane calculator calculator; }
cpp_contacts() { cpp_lane contacts contacts; }
cpp_events() { cpp_lane events events; }
cpp_kvstore() { cpp_lane kvstore kvstore; }
cpp_async_demo() { cpp_lane async-demo async_demo; }
cpp_codec() { cpp_lane codec codec; }

lane cpp-calculator cpp_calculator
lane cpp-contacts cpp_contacts
lane cpp-events cpp_events
lane cpp-kvstore cpp_kvstore
lane cpp-async-demo cpp_async_demo
lane cpp-codec cpp_codec

finish_lanes
