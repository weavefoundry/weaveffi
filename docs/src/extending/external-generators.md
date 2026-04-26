# External Generators

WeaveFFI ships eleven built-in generators (`c`, `cpp`, `swift`, `android`,
`node`, `wasm`, `python`, `dotnet`, `dart`, `go`, `ruby`). To emit bindings
for languages or toolchains we do not ship, you can drop a standalone
executable on `$PATH` and WeaveFFI will invoke it on demand.

## Discovery

On every `weaveffi generate` invocation the CLI walks each directory in
`$PATH` and records executables whose filename:

- starts with the prefix `weaveffi-gen-`,
- has at least one character after the prefix,
- is a regular file with the executable bit set (on Unix), and
- does **not** match a built-in target name
  (`c`, `cpp`, `swift`, `android`, `node`, `wasm`, `python`, `dotnet`,
  `dart`, `go`, `ruby`).

The suffix after `weaveffi-gen-` becomes the target name. For example,
`weaveffi-gen-kotlin-multiplatform` registers `kotlin-multiplatform` as a
valid `--target` value.

The first binary found for a given suffix wins, matching shell resolution
order. Later entries with the same name are ignored.

## Invocation

When `--target` includes a discovered name, WeaveFFI:

1. Validates the API against the IR schema.
2. Serialises the validated [`Api`](../reference/idl.md) to a temporary
   JSON file.
3. Creates `<out_dir>/<name>/`.
4. Executes the binary:

   ```sh
   weaveffi-gen-<name> --api <api.json> --out <out_dir>/<name>
   ```

The child inherits the parent process's environment, `stdin`, `stdout`,
and `stderr`. Both `--api` and `--out` are absolute paths.

## Protocol

### stdin

Binaries should not read from `stdin`. WeaveFFI does not write anything
on it; treat it as closed.

### stdout and stderr

The binary owns both streams. Diagnostic output, progress, warnings, and
errors should be written to `stderr` in plain text. `stdout` is reserved
for future structured output; avoid emitting anything that would confuse
scripts parsing WeaveFFI's own output.

### Exit codes

| Code    | Meaning                                                        |
| ------- | -------------------------------------------------------------- |
| `0`     | Generation succeeded; every file was written.                  |
| non-`0` | Generation failed. WeaveFFI aborts with the same status code.  |

### Directory layout

All emitted files must live under the `--out` directory that WeaveFFI
passed on the command line. Binaries must not touch files outside that
tree. WeaveFFI creates the directory before invoking the binary; the
binary is free to create nested subdirectories within it.

```text
<out_dir>/
└── <name>/                   <-- --out points here
    ├── README.md
    ├── src/
    │   └── ...generated sources...
    └── ...
```

## Version negotiation

Binaries must accept an `--abi-version` flag. When invoked with only
`--abi-version`, the binary prints the IR schema versions it supports to
`stdout`, one per line, then exits with status `0`:

```console
$ weaveffi-gen-custom --abi-version
0.1.0
0.2.0
```

WeaveFFI compares those versions against the one baked into the CLI (see
`weaveffi schema-version`). Binaries that do not implement
`--abi-version`, or whose supported set does not include WeaveFFI's
current schema version, are considered incompatible and may be rejected
by future releases.

## Minimal example

A minimum-viable external generator written in POSIX shell:

```sh
#!/bin/sh
set -eu

if [ "${1-}" = "--abi-version" ]; then
  echo "0.2.0"
  exit 0
fi

api=""
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --api) api="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) echo "unknown arg: $1" 1>&2; exit 2 ;;
  esac
done

[ -n "$api" ] || { echo "missing --api" 1>&2; exit 2; }
[ -n "$out" ] || { echo "missing --out" 1>&2; exit 2; }

# Emit a placeholder artefact derived from the API payload.
mkdir -p "$out"
cp "$api" "$out/api.json"
```

Save the script as `weaveffi-gen-demo`, `chmod +x` it, put it on `$PATH`,
and run:

```sh
weaveffi generate weaveffi.yml -o generated --target demo
```

WeaveFFI will emit built-in targets as normal and additionally invoke
`weaveffi-gen-demo`, which copies the API payload into
`generated/demo/api.json`.
