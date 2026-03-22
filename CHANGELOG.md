## [0.1.0](https://github.com/weavefoundry/weaveffi/compare/v0.0.0...v0.1.0) (2026-03-22)

### ⚠ BREAKING CHANGES

* correct string ABI and naming mismatches across generators and samples
* add validation rules for enum definitions and type references
* add validation rules for struct definitions
* add Optional and List type wrappers to TypeRef
* add enum definitions to IR model
* add struct definitions to IR model

### Features

* add --scaffold flag to generate Rust FFI function stubs ([94cab31](https://github.com/weavefoundry/weaveffi/commit/94cab31237937c8b0df6acadec8afc4145bc9920))
* add --target flag to filter generators in generate command ([0857b30](https://github.com/weavefoundry/weaveffi/commit/0857b300e6ce5f370b155056b8853c5f57deadd7))
* add contacts consumer examples for C, Node, and Swift ([51cdabf](https://github.com/weavefoundry/weaveffi/commit/51cdabf97a5aefb0690e1307d9f6ed31ea1617d0))
* add contacts sample IR definition with full type coverage ([757baca](https://github.com/weavefoundry/weaveffi/commit/757baca9a24a73384fb1a774b70ceb9b239bbb25))
* add contacts sample Rust library with C ABI bindings ([1dd2d23](https://github.com/weavefoundry/weaveffi/commit/1dd2d2397f9c24570842541370644994eed2239e))
* add enum definitions to IR model ([a7a25c8](https://github.com/weavefoundry/weaveffi/commit/a7a25c83068326eb6f06f2fb1f857f1fd0c53c88))
* add enum, optional, and list support to Android generator ([6824475](https://github.com/weavefoundry/weaveffi/commit/6824475119e3cb92493ce63a0735b364f9b66930))
* add enum, optional, and list support to C generator ([bf7f0c1](https://github.com/weavefoundry/weaveffi/commit/bf7f0c159e737fccb386ae9f4f2238e90a5400dd))
* add enum, optional, and list support to Swift generator ([453076d](https://github.com/weavefoundry/weaveffi/commit/453076d05ddeef3d4122c8dbfa5b5fc78afd8a63))
* add Optional and List type wrappers to TypeRef ([5520fbc](https://github.com/weavefoundry/weaveffi/commit/5520fbce4e3312c98444cb2fd8d15b6a1c854ad6))
* add source context and suggestions to CLI error messages ([ebd9c7d](https://github.com/weavefoundry/weaveffi/commit/ebd9c7d67659ad9b3d95708148c6dab9191e2c34))
* add struct definitions to IR model ([3a5fd77](https://github.com/weavefoundry/weaveffi/commit/3a5fd773841c32c9e2970bf934c392be4ca10362))
* add struct support to Android/Kotlin generator ([cbed3c1](https://github.com/weavefoundry/weaveffi/commit/cbed3c194af4b8337305eef04f2d960128966bc6))
* add struct support to C generator ([415e88a](https://github.com/weavefoundry/weaveffi/commit/415e88a3eb180b3d43234e440afd34356ef27aa8))
* add struct support to Swift generator ([c0fb4d5](https://github.com/weavefoundry/weaveffi/commit/c0fb4d5e7e980df649719a5f0bc0953a7f37481f))
* add struct, enum, optional, and list support to Node generator ([a3aa897](https://github.com/weavefoundry/weaveffi/commit/a3aa8972624b5f2076c70851b2f1ee8098d2ff6f))
* add validate subcommand to CLI ([7261dd6](https://github.com/weavefoundry/weaveffi/commit/7261dd6c46d309cb9fc9b0abfda7696a47435f81))
* add validation rules for enum definitions and type references ([10a8e19](https://github.com/weavefoundry/weaveffi/commit/10a8e1934b64f7d6a0b1df2f741235f690f88876))
* add validation rules for struct definitions ([0ad5c9f](https://github.com/weavefoundry/weaveffi/commit/0ad5c9f62bcf4cfe7eb4d6d9705fc0701ad94931))

### Bug Fixes

* add retry logic and duplicate handling to crate publish script ([e446700](https://github.com/weavefoundry/weaveffi/commit/e4467008b0864042db3a2ecdc03eb46f2a464231))
* correct string ABI and naming mismatches across generators and samples ([1fb9553](https://github.com/weavefoundry/weaveffi/commit/1fb9553c7616aa7eb14b1e19dec549918f873f1f))
* resolve clippy single-char-add-str warning in WASM generator ([4ec0caf](https://github.com/weavefoundry/weaveffi/commit/4ec0cafc72a407156f8527b109a976ddf2e20936))
* shorten weaveffi-ir keyword to satisfy crates.io 20-char limit ([b3630b7](https://github.com/weavefoundry/weaveffi/commit/b3630b7211d8bcb2acebac28e3d67dcda8330a13))
