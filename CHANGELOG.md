## [0.4.0](https://github.com/weavefoundry/weaveffi/compare/v0.3.0...v0.4.0) (2026-05-05)

### ⚠ BREAKING CHANGES

* add prelude header and trailer marker to generator output
* audit and balance async callback lifetimes across generators
* emit doc strings in every generator with native syntax
* enforce deterministic iteration in IR and cache hashing
* support every GeneratorConfig field via inline IDL generators
* add weaveffi upgrade subcommand and bump schema to 0.3.0
* drop TypeRef::Callback in favor of module-level callbacks

### Features

* add cargo-fuzz harnesses for parsers and validator ([0766e2c](https://github.com/weavefoundry/weaveffi/commit/0766e2c07c502373bf3d5947ccfac9e2a6e95c00))
* add diff --check and --format json for validate and lint ([02ef6ae](https://github.com/weavefoundry/weaveffi/commit/02ef6aeb379e0fca885695366bf8498bd4053a22))
* add kvstore sample exercising every IDL feature ([f05ffa2](https://github.com/weavefoundry/weaveffi/commit/f05ffa25b3b793e942eb5e0ec082a33b748eb7a0))
* add prelude header and trailer marker to generator output ([039c66e](https://github.com/weavefoundry/weaveffi/commit/039c66e4469e4704fcb9129b9a09a16d2833446c))
* add target filter and JSON output to weaveffi doctor ([7cb83c8](https://github.com/weavefoundry/weaveffi/commit/7cb83c8dfda731f0d34c090082b8dd14494d5fe9))
* add watch, format, and JSON schema export commands ([f067b86](https://github.com/weavefoundry/weaveffi/commit/f067b867ff2cf255d83feed5ee74e9673ff69b72))
* add weaveffi upgrade subcommand and bump schema to 0.3.0 ([707d99a](https://github.com/weavefoundry/weaveffi/commit/707d99abd5aaca4cee668ac920d8b2b13df4a188))
* emit doc strings in every generator with native syntax ([f592388](https://github.com/weavefoundry/weaveffi/commit/f5923886560d95e7b9b09275d9566a9085d204e7))
* extract typed handles, async, listeners, deprecated and mutable refs ([7369415](https://github.com/weavefoundry/weaveffi/commit/7369415130013a6a3d24339f810f9b1fb7e57e67))
* integrate miette for span-aware diagnostics ([eb0595c](https://github.com/weavefoundry/weaveffi/commit/eb0595c5c0f6173a5710958102b417efe8d1886a))
* support every GeneratorConfig field via inline IDL generators ([102016e](https://github.com/weavefoundry/weaveffi/commit/102016ed1c52bb0eed52ff6d9894f948fc19deef))
* thread c_prefix through C, C++, and scaffold output ([68d315f](https://github.com/weavefoundry/weaveffi/commit/68d315fb8b57ffb477a3b00f0a4e931ede289c6c))

### Bug Fixes

* align Android JNI call sites with C ABI, and seed Go example go.sum ([3c3a01e](https://github.com/weavefoundry/weaveffi/commit/3c3a01e2e981c84053896cb534998c6c60bcf3b9))
* apply inline generators when computing weaveffi diff output ([52bc194](https://github.com/weavefoundry/weaveffi/commit/52bc1949adfc09b795e4c58832cdd09e0111beff))
* audit and balance async callback lifetimes across generators ([076bc11](https://github.com/weavefoundry/weaveffi/commit/076bc117460b930a4c0cdd916148c71cd138c9c7))
* enforce deterministic iteration in IR and cache hashing ([b57b1c9](https://github.com/weavefoundry/weaveffi/commit/b57b1c99ed15df2cbc75f44c3a8e09542c397435))
* wire List<String> JNI parameters end-to-end, and stub Iterator returns in Android generator ([d9b32f6](https://github.com/weavefoundry/weaveffi/commit/d9b32f64789a9ae1d15b858cabc00d3fec995736))

### Performance

* parallelize orchestrator with per-generator cache invalidation ([306582b](https://github.com/weavefoundry/weaveffi/commit/306582bc92ff46124bfec5207c5f280c21830744))
* pre-allocate generator buffers and add performance targets ([982cc70](https://github.com/weavefoundry/weaveffi/commit/982cc70b0f3251f4c095a15bffa173ae0b1a0fa2))

### Code Refactoring

* drop TypeRef::Callback in favor of module-level callbacks ([6c76e93](https://github.com/weavefoundry/weaveffi/commit/6c76e93caf1c90485a13dfc91464cad4034106f4))

## [0.3.0](https://github.com/weavefoundry/weaveffi/compare/v0.2.0...v0.3.0) (2026-04-01)

### ⚠ BREAKING CHANGES

* remove premature backwards compatibility machinery

### Features

* add --templates flag to generate command for user template overrides ([ee3e1db](https://github.com/weavefoundry/weaveffi/commit/ee3e1db4a98f740ef8281554094149105dd05bae))
* add arena module for batch handle management in weaveffi-abi ([7ee8651](https://github.com/weavefoundry/weaveffi/commit/7ee865189cd4160741775cf763673050c9838e69))
* add async C ABI convention with callback typedef and signature ([9eec6bb](https://github.com/weavefoundry/weaveffi/commit/9eec6bb7e36fb61dabfb441ab0a0b14399bd1e6e))
* add async function support to scaffold generator ([87608bf](https://github.com/weavefoundry/weaveffi/commit/87608bf60b378ee8324b989de27f2c1eb442eb7f))
* add async Promise support to WASM generator ([76d9c03](https://github.com/weavefoundry/weaveffi/commit/76d9c03cd92bf753392c93b5c3f9e67f41f160b4))
* add async std::future/std::promise support to C++ generator ([450c38f](https://github.com/weavefoundry/weaveffi/commit/450c38f8370449c51c07bbb1981eded5b4eb7cb6))
* add async support and CallbackDef type to IR ([c7fdc5d](https://github.com/weavefoundry/weaveffi/commit/c7fdc5d0c454b1685080313ec6180f957bde8219))
* add async-demo sample Rust library with C ABI async functions ([effb3ca](https://github.com/weavefoundry/weaveffi/commit/effb3ca63e7dbae5df6471e7d7122a5f943f02a5))
* add async/await support to Swift generator ([8181254](https://github.com/weavefoundry/weaveffi/commit/8181254795a8e8f5bb4f43b3be7a20588da598ce))
* add asyncio support to Python generator for async functions ([5841a36](https://github.com/weavefoundry/weaveffi/commit/5841a36dd199dbab50054c26a91275af3c650557))
* add BorrowedStr and BorrowedBytes types for zero-copy FFI parameters ([6df6c5b](https://github.com/weavefoundry/weaveffi/commit/6df6c5b74214f11eb0146868c71214c66bf52e49))
* add builder pattern support to IR and all generators ([7e51fe0](https://github.com/weavefoundry/weaveffi/commit/7e51fe01b7522b4c99877db6714130ce43c70705))
* add C++ config options to GeneratorConfig ([f566ae5](https://github.com/weavefoundry/weaveffi/commit/f566ae5bf007bd3ae5f9fb5349c227e6b28b457f))
* add callback/event listener pattern to IR with validation and C codegen ([80524d0](https://github.com/weavefoundry/weaveffi/commit/80524d0ddc376433ddb9117a0b264c508b28ecbe))
* add cancellation token support across all FFI generators ([ad90901](https://github.com/weavefoundry/weaveffi/commit/ad909013bbcb75f556c4754c3fa9b308b5c16b90))
* add completions subcommand to CLI ([c837a51](https://github.com/weavefoundry/weaveffi/commit/c837a51810fdeb9c4f4ec82b3ab2dc353be7e13b))
* add const annotations for mutable parameter support ([b572fae](https://github.com/weavefoundry/weaveffi/commit/b572faed5dde446a298ba5c502444387709d08bf))
* add coroutine support to Kotlin/Android generator ([10d1c77](https://github.com/weavefoundry/weaveffi/commit/10d1c7787b73cff60859f5df1fccba043b0fb331))
* add cross-module add_product_to_order function to inventory sample ([3f9a7eb](https://github.com/weavefoundry/weaveffi/commit/3f9a7eb995090274bdcd694334593b8df2b42911))
* add cross-module type resolution to the validator ([af8f7ac](https://github.com/weavefoundry/weaveffi/commit/af8f7acb9ae81f9634e232c23c71e425c25b1f67))
* add Dart config and comprehensive generator tests ([8f4d3ef](https://github.com/weavefoundry/weaveffi/commit/8f4d3ef61a52ab598bca280a519e729fc9cdf4ae))
* add error handling to WASM JS function wrappers ([00914e8](https://github.com/weavefoundry/weaveffi/commit/00914e801c7aef12f1dfa481827a66a79a3d9b82))
* add Go generator config and comprehensive tests ([5b10e2e](https://github.com/weavefoundry/weaveffi/commit/5b10e2ea2d17f59400fba573a4d456b101399eef))
* add inline [generators] section support to IDL files ([c8ead4d](https://github.com/weavefoundry/weaveffi/commit/c8ead4db51f02ff2164eeeece586fa981d6912d6))
* add IR schema version checking to validator ([57c0d78](https://github.com/weavefoundry/weaveffi/commit/57c0d78e927564ca3e30db167a4b1865921aa564))
* add iterator/streaming pattern to IR and all generators ([121ad44](https://github.com/weavefoundry/weaveffi/commit/121ad446d064d397af5efe68ac2a8f3767248e7b))
* add listener/streaming events sample ([d1b2ee8](https://github.com/weavefoundry/weaveffi/commit/d1b2ee85a24e670fb5fed16e9892bcae9774af76))
* add nested module support to all generators ([2fdf93b](https://github.com/weavefoundry/weaveffi/commit/2fdf93b3f6059470a6f68b4bda6ec6e3a41d6fa6))
* add nested module support to IR, validator, and C generator ([33bd2f7](https://github.com/weavefoundry/weaveffi/commit/33bd2f7483de8ef2fa77691b0f004ca9bb9da0a2))
* add pre- and post-generation hook commands to Orchestrator ([d7d08a6](https://github.com/weavefoundry/weaveffi/commit/d7d08a69a8df46d614408cae31f6d9538cc4d69e))
* add Promise support to Node generator for async functions ([28e147a](https://github.com/weavefoundry/weaveffi/commit/28e147a1835a34496daec79bb718c666730643e3))
* add python_package_name and dotnet_namespace to GeneratorConfig ([6bdf0ee](https://github.com/weavefoundry/weaveffi/commit/6bdf0eea2727d2013d4b0bbfeff100c038807ed5))
* add Ruby generator config and comprehensive tests ([51aaeac](https://github.com/weavefoundry/weaveffi/commit/51aaeacc64781c2b27fd24475ac1af3663a53534))
* add schema-version subcommand to print current IR version ([dfe4740](https://github.com/weavefoundry/weaveffi/commit/dfe4740df46174fada97b8277a59922593bbf351))
* add string.h include and free_string test to Node addon ([98c6cbc](https://github.com/weavefoundry/weaveffi/commit/98c6cbc9d0f2539876637e1a17aa7dc07154b3bc))
* add Task support to .NET generator for async functions ([5635156](https://github.com/weavefoundry/weaveffi/commit/56351564d4e0a9827f1576e15235b8fcabdbde90))
* add template context builders for API model ([c1c70b9](https://github.com/weavefoundry/weaveffi/commit/c1c70b9f58569debc6f6e96610d6e0be405b2b46))
* add tera template engine to weaveffi-core ([72dd085](https://github.com/weavefoundry/weaveffi/commit/72dd08533667ab892e0ddb75a4fade2142179c73))
* add TypedHandle support to all generators ([c8a1f30](https://github.com/weavefoundry/weaveffi/commit/c8a1f30cee8185e181b1f2c06fc9fea35c8bb236))
* add TypedHandle support to scaffold generator ([3a14f42](https://github.com/weavefoundry/weaveffi/commit/3a14f42613b21044022eeb71906a78f861fd7e03))
* add TypedHandle(String) variant to TypeRef for typed handle references ([1032c20](https://github.com/weavefoundry/weaveffi/commit/1032c20d0e09d01381bdbbdef21ab9a774a12f99))
* add upgrade subcommand for IR schema migration ([8929a47](https://github.com/weavefoundry/weaveffi/commit/8929a471431e18fbc5a0ca14ee1a273086dc3813))
* add versioned API evolution with deprecation annotations ([023c107](https://github.com/weavefoundry/weaveffi/commit/023c10709c49bf0c7c0a9080711fb57cdd8398f4))
* add weaveffi-gen-cpp crate with placeholder C++ header generator ([21b7935](https://github.com/weavefoundry/weaveffi/commit/21b7935d7bd3ca356cf848a3a821be22aec225f0))
* add weaveffi-gen-dart crate with dart:ffi binding generator ([b658992](https://github.com/weavefoundry/weaveffi/commit/b6589920e8862f80f55ac87107cbd2efd2b87282))
* add weaveffi-gen-go crate with CGo binding generator ([12c4250](https://github.com/weavefoundry/weaveffi/commit/12c4250c4218e38ca390c7a6aaab345173f94f2e))
* add weaveffi-gen-ruby crate with Ruby FFI binding generator ([67325e5](https://github.com/weavefoundry/weaveffi/commit/67325e5cc9da5c4819ab5ab4b3ca1e2f9e75a223))
* add WeaveFFIError exception class to C++ generator ([8d2299b](https://github.com/weavefoundry/weaveffi/commit/8d2299b7dbcd995a48d7253e139110416b3c098c))
* generate CMakeLists.txt and README for C++ consumer projects ([14cc9b7](https://github.com/weavefoundry/weaveffi/commit/14cc9b78e3f50821202221988b68ca2cec5219df))
* generate Dart pubspec.yaml and README.md packaging scaffold ([f320c44](https://github.com/weavefoundry/weaveffi/commit/f320c441f20818e0c7d53f070dcac358895b702a))
* generate gemspec and README packaging scaffold for Ruby bindings ([a5aac22](https://github.com/weavefoundry/weaveffi/commit/a5aac221f2f47af5fdebcbc0ba920de4fbb415cd))
* generate go.mod and README.md packaging scaffold for Go bindings ([9070698](https://github.com/weavefoundry/weaveffi/commit/9070698db377f76f8c5ac84f120526f9f625e250))
* handle qualified cross-module struct type names in all generators ([285c4c7](https://github.com/weavefoundry/weaveffi/commit/285c4c7b5a0fcf997bb9f39119c6d656d0dc9963))
* implement C++ header generation with RAII wrappers ([7adfd2d](https://github.com/weavefoundry/weaveffi/commit/7adfd2d023d2fd8758ea6e648141ce36ee8c832a))
* implement functional N-API bodies in Node generator ([be3f615](https://github.com/weavefoundry/weaveffi/commit/be3f6153f8bde5914d47e771b60d45dcd6f12622))
* improve weaveffi new scaffold with glob import and test instructions ([4c47366](https://github.com/weavefoundry/weaveffi/commit/4c47366beece0c7c535c3a0802d187ee6e01b583))
* scaffold complete working project from weaveffi new ([63a483d](https://github.com/weavefoundry/weaveffi/commit/63a483de3d36a7d0aea1e58fc05de047d2b7f371))
* update deprecated handle warning to suggest handle<StructName> syntax ([09fa3a6](https://github.com/weavefoundry/weaveffi/commit/09fa3a6b6cb69e56daeb1680e4866e7e7b10350f))
* wire c_prefix config into C generator output_files ([6de8256](https://github.com/weavefoundry/weaveffi/commit/6de825632a525e28dfe769123b99bad8718d67f3))
* wire c_prefix config into the C generator ([5c6a4bb](https://github.com/weavefoundry/weaveffi/commit/5c6a4bbf8cf973e7da85e795bc126f98baf44d36))
* wire strip_module_prefix into all generators ([92b5cc5](https://github.com/weavefoundry/weaveffi/commit/92b5cc5b2516ad3dce64d39976e0e6c828439351))
* wire wasm_module_name config into WASM generator ([9235845](https://github.com/weavefoundry/weaveffi/commit/9235845d4fcf94c8a9457c9f7a2b90856b2013cd))
* wire wasm_module_name config into WASM generator output_files ([4657d87](https://github.com/weavefoundry/weaveffi/commit/4657d873573606cd82ad511deb6e147d9b3ffbac))

### Bug Fixes

* add missing generators field to benchmark Api constructors ([8d168a7](https://github.com/weavefoundry/weaveffi/commit/8d168a713eab16e8789a3cf6bce8893e696f9c01))
* complete codegen benchmarks with all generators and missing field ([4c9eeb3](https://github.com/weavefoundry/weaveffi/commit/4c9eeb368a9d10012cfb097a8ad95844a751f3d1))
* remove stale version reference from AsyncNotSupported error message ([ef2e225](https://github.com/weavefoundry/weaveffi/commit/ef2e2255c9ae65556faff9cfb7599d951b1a6e64))
* resolve clippy unnecessary_unwrap warning in Dart generator ([916c53e](https://github.com/weavefoundry/weaveffi/commit/916c53e5497676871991a0cb7f6018e3e418ebf0))
* resolve clippy warnings across workspace ([53c7755](https://github.com/weavefoundry/weaveffi/commit/53c77550db3f0233c9387dbd77d49433a787d605))
* resolve Windows path separators and macOS SIGSEGV in tests ([33625d1](https://github.com/weavefoundry/weaveffi/commit/33625d19dbae60c0a8e2f3dde963b78dd8bcfdad))
* use per-test counters in arena tests to prevent parallel flakiness ([ca7ba1d](https://github.com/weavefoundry/weaveffi/commit/ca7ba1d64fefc15766f6b28ebb6e7c7e80ec3b9e))
* use platform-aware paths in generator output_files tests ([771ef20](https://github.com/weavefoundry/weaveffi/commit/771ef20f6b5d38007286b14330a1a410537a19c9))

### Code Refactoring

* remove premature backwards compatibility machinery ([0c7a476](https://github.com/weavefoundry/weaveffi/commit/0c7a476fb0e1165840663929b97381d63a71afb5))

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
