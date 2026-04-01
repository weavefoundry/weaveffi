## [0.2.0](https://github.com/weavefoundry/weaveffi/compare/v0.1.0...v0.2.0) (2026-04-01)

### ⚠ BREAKING CHANGES

* integrate incremental codegen into Orchestrator with --force flag
* add --config flag to generate command for TOML generator options
* use typed enum params and returns in Kotlin function signatures
* add map key type validation rejecting struct, list, and map keys
* add Map(Box<TypeRef>, Box<TypeRef>) variant to TypeRef
* add resolve_type_refs to fix Struct refs that should be Enum refs

### Features

* add --config flag to generate command for TOML generator options ([c8d6ba2](https://github.com/weavefoundry/weaveffi/commit/c8d6ba26eb70ab9bfc74f7270f3e6e26e7505d6e))
* add --dry-run flag to generate command ([b6ad30f](https://github.com/weavefoundry/weaveffi/commit/b6ad30fe37c05b6d9efc8acfaf973b4a6df6357e))
* add --quiet and --verbose flags to CLI ([29607bc](https://github.com/weavefoundry/weaveffi/commit/29607bcfa9dd5c1e11371713756f14dfe0881c6d))
* add --warn flag to validate and generate commands ([3490554](https://github.com/weavefoundry/weaveffi/commit/3490554a69301c3e4af4f23d6e9306a3fccc53e8))
* add binding.gyp and N-API addon stub to Node generator ([b7b78e8](https://github.com/weavefoundry/weaveffi/commit/b7b78e8de9d587286f86fc90dee8c237cddcde12))
* add content-hashing cache module to weaveffi-core ([b2ca6ec](https://github.com/weavefoundry/weaveffi/commit/b2ca6ec9e04704d8642d26b8598bc365a10e54ec))
* add cross-compilation target and wasm tool checks to doctor command ([cd6226d](https://github.com/weavefoundry/weaveffi/commit/cd6226d2dd63c686fde99331755098cdee53c5d1))
* add diff subcommand to compare generated bindings ([43fdeca](https://github.com/weavefoundry/weaveffi/commit/43fdeca671316686fff1c8f2b7b3ec48d0821e4d))
* add externalNativeBuild cmake config to Android build.gradle ([9d7d914](https://github.com/weavefoundry/weaveffi/commit/9d7d9143bd8c1c4f6f92bf8f807165183566ea6b))
* add extract module to parse Rust source into API IR ([b7a4f25](https://github.com/weavefoundry/weaveffi/commit/b7a4f2537a4f9f06b46509c1fec130f69358edaf))
* add extract subcommand to CLI ([38489af](https://github.com/weavefoundry/weaveffi/commit/38489af6eeaf1f5b6bb143e39dcf9c083b32a7cb))
* add GeneratorConfig struct to weaveffi-core ([302d302](https://github.com/weavefoundry/weaveffi/commit/302d3021deeaadfc15378c94f0f511244f63df62))
* add inline error types to Kotlin generator ([09ab5a5](https://github.com/weavefoundry/weaveffi/commit/09ab5a537fbfc6256124e54d6fe64e1aeccdeba5))
* add inline error types to Swift generator ([b874045](https://github.com/weavefoundry/weaveffi/commit/b874045b3c0bfd02fca7d7cff09eb1045737bdc6))
* add inline memory helpers to .NET generator ([f48b0ef](https://github.com/weavefoundry/weaveffi/commit/f48b0ef2828f664bad5f7d111c8cdd6518b5127b))
* add inline memory helpers to Python generator ([ff9df37](https://github.com/weavefoundry/weaveffi/commit/ff9df375b0d03e38e80803728df477a3ce5578b4))
* add inventory sample IR with multi-module definition ([5642b89](https://github.com/weavefoundry/weaveffi/commit/5642b89b08066301b9f793070baf42e058467e92))
* add inventory sample Rust library with C ABI bindings ([b42ffe9](https://github.com/weavefoundry/weaveffi/commit/b42ffe98471b0c5daf9f834fffcd192f00e2625d))
* add JSDoc comments mapping C ABI functions in Node types.d.ts ([6e584b5](https://github.com/weavefoundry/weaveffi/commit/6e584b5deed4783fd9e5af91c6d93614c815a403))
* add lint subcommand for CI-enforced warning-free IDL files ([f98637a](https://github.com/weavefoundry/weaveffi/commit/f98637ac5ca9c4afc1564f2f5bef6817e83a9409))
* add map key type validation rejecting struct, list, and map keys ([786057f](https://github.com/weavefoundry/weaveffi/commit/786057fab4889ec03e24f6d29114ce66016bd03d))
* add Map type support to Android/Kotlin generator ([e14efc2](https://github.com/weavefoundry/weaveffi/commit/e14efc29944f85e97a1a6fd4318114c2c065a734))
* add Map type support to C generator with parallel-arrays convention ([cb58969](https://github.com/weavefoundry/weaveffi/commit/cb589697fe72ea2d1a2ffccce5abb1941ad0ebfc))
* add Map type support to Swift generator ([b2dceb7](https://github.com/weavefoundry/weaveffi/commit/b2dceb7ea82a0b6ce728c10178ec189f9a30dddb))
* add Map(Box<TypeRef>, Box<TypeRef>) variant to TypeRef ([25e1440](https://github.com/weavefoundry/weaveffi/commit/25e1440c01f062280c719444d6cd0773c99fe6ac))
* add Optional and List field getter support in Swift struct codegen ([0dc4732](https://github.com/weavefoundry/weaveffi/commit/0dc4732ee9a910079bb1372fb1aa38c7cf0bb51c))
* add resolve_type_refs to fix Struct refs that should be Enum refs ([dc7f25c](https://github.com/weavefoundry/weaveffi/commit/dc7f25c75a38d55f0a0724e0ffa572f807fe2304))
* add ValidationWarning type and collect_warnings for non-fatal issues ([429e0d1](https://github.com/weavefoundry/weaveffi/commit/429e0d1d765fa66e0a951e6517511b0d4906bc2a))
* add weaveffi-gen-dotnet crate with C# P/Invoke binding generator ([9d2910a](https://github.com/weavefoundry/weaveffi/commit/9d2910a32ca9fe2cff1bed54f931f7ccc6883352))
* add weaveffi-gen-python crate with placeholder generator ([27f972a](https://github.com/weavefoundry/weaveffi/commit/27f972ad2732bf2292cca6f3337e5ac56979574c))
* generate .NET project scaffold (csproj, nuspec, README) ([83d6c3e](https://github.com/weavefoundry/weaveffi/commit/83d6c3ef71d8d9424873f74a6e7d0530c0223d2c))
* generate API-specific JS wrappers in WASM stub ([c5b8e15](https://github.com/weavefoundry/weaveffi/commit/c5b8e15b465b03aed76a72e23380e30c85230319))
* generate PEP 484 type stubs for Python bindings ([0cd0c17](https://github.com/weavefoundry/weaveffi/commit/0cd0c179ffb9fd94ef101b9d14a347549eec6cfc))
* generate Python packaging scaffold (pyproject.toml, setup.py, README) ([ae82002](https://github.com/weavefoundry/weaveffi/commit/ae8200238d08ec2b43b92b034ddbc65baaf1e847))
* generate TypeScript declaration file in WASM generator ([b29c0b1](https://github.com/weavefoundry/weaveffi/commit/b29c0b13cba5c49af7938d7a1bc7b47e3536c15a))
* implement .NET P/Invoke binding generation with IDisposable structs ([983d88a](https://github.com/weavefoundry/weaveffi/commit/983d88ade06b2f0f2102acfd7ad4e457ed730720))
* implement Python ctypes binding generation ([4a1223e](https://github.com/weavefoundry/weaveffi/commit/4a1223e26cd9542d38bea423ed85fb81428681e1))
* integrate incremental codegen into Orchestrator with --force flag ([5c3c05c](https://github.com/weavefoundry/weaveffi/commit/5c3c05c6986dd187755fc41c7376e17bcaf92a9b))
* make WASM generator API-driven with type-mapped API reference ([4208304](https://github.com/weavefoundry/weaveffi/commit/42083044d3080ee446c7b3c43a5ac0ec368e44d4))
* respect GeneratorConfig.android_package in Android generator ([e5863e7](https://github.com/weavefoundry/weaveffi/commit/e5863e73559c58783145e437e42df08f9f92778a))
* respect GeneratorConfig.node_package_name in Node generator ([aa7dff3](https://github.com/weavefoundry/weaveffi/commit/aa7dff319d52944dc63b20e42dab846f60036aa3))
* respect GeneratorConfig.swift_module_name in Swift generator ([75f0cf5](https://github.com/weavefoundry/weaveffi/commit/75f0cf5419547d7d35b479faa0bb2f74d9e42e16))
* scaffold Map params as parallel arrays and returns as out-params ([01688b8](https://github.com/weavefoundry/weaveffi/commit/01688b8f503e192899e569f51421e1d8147bad90))
* use typed enum params and returns in Kotlin function signatures ([d675fc5](https://github.com/weavefoundry/weaveffi/commit/d675fc548e8f5f13fbaf3effe197b85486577dd7))

### Bug Fixes

* resolve clippy warnings for redundant closure and unnecessary map_or ([d5f2388](https://github.com/weavefoundry/weaveffi/commit/d5f2388b4dac04eea0fe2b63c9816da4c65c0ad7))

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
