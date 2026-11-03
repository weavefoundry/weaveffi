#!/usr/bin/env bash
# Conformance lanes for the python target. Run through conformance/run.sh,
# which builds the sample producers, generates bindings, and exports the
# shared environment (see conformance/lib.sh); each lane runs one consumer
# against one sample and must exit 0.
set -uo pipefail
. "$(dirname "$0")/../lib.sh"
. "$ROOT/scripts/toolchains.sh"
PYTHON=$(find_python) || require_tools python python3.10
export PYTHON

# Run a Python consumer against the generated package as-is: its directory
# goes on PYTHONPATH, and the producer cdylib is selected through the
# loader's `{PREFIX}_LIBRARY` override (macOS strips DYLD_LIBRARY_PATH from
# the system python3).
py_consumer() {
    local sample="$1" script="$2"
    env PYTHONPATH="$GENROOT/$sample/python" \
        "$(library_env "$sample")=$(sample_lib "$sample")" \
        "$PYTHON" -X dev "$ROOT/conformance/python/$script"
}

python_calculator() { py_consumer calculator calculator_consumer.py; }
python_codec() { py_consumer codec codec_consumer.py; }
python_kvstore() { py_consumer kvstore kvstore_consumer.py; }

lane python-calculator python_calculator
lane python-codec python_codec
lane python-kvstore python_kvstore

finish_lanes
