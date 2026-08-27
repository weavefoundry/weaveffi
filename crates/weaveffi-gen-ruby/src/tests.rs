//! Tests for the Ruby generator: rendering coverage across every entity
//! and call shape, error-domain semantics, packaging, and identifier policy.

use super::*;
use crate::types::{rb_abi_types, rb_ffi_type};
use camino::Utf8Path;
use weaveffi_core::abi;
use weaveffi_core::codegen::Generator;

#[test]
fn package_emits_platform_gems_and_swaps_loader() {
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = make_api(vec![simple_module(
        "calc",
        vec![Function {
            name: "ping".into(),
            params: vec![],
            returns: None,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
    bins.insert(Platform::LinuxX64, "/s/linux-x64/libcalculator.so");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    let files = LanguageBackend::package(
        &RubyGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &RubyConfig::default(),
    )
    .expect("ruby supports packaging");

    assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
    // Bundled under lib/native/ inside each per-platform gem dir.
    assert!(files.iter().any(|f| f
        .path
        .as_str()
        .ends_with("ruby/darwin-arm64/lib/native/libcalculator.dylib")));
    // The gemspec stamps the RubyGems platform string.
    let gemspec = files
        .iter()
        .find(|f| f.path.as_str().ends_with("darwin-arm64/weaveffi.gemspec"))
        .expect("gemspec present");
    let FileContent::Text(spec) = &gemspec.content else {
        panic!("gemspec is text");
    };
    assert!(
        spec.contains("s.platform    = 'arm64-darwin'"),
        "platform: {spec}"
    );
    // The loader was rewritten to prefer the bundled library.
    let rb = files
        .iter()
        .find(|f| f.path.as_str().ends_with("darwin-arm64/lib/weaveffi.rb"))
        .expect("library module present");
    let FileContent::Text(src) = &rb.content else {
        panic!("module is text");
    };
    assert!(
        src.contains("File.exist?") && src.contains("libcalculator.dylib"),
        "packaged loader not applied: {src}"
    );
}
use weaveffi_ir::ir::{
    Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef, Module, Param,
    StructDef, StructField, TypeRef,
};

fn make_api(modules: Vec<Module>) -> Api {
    Api {
        version: "0.6.0".to_string(),
        modules,
        generators: None,
        package: None,
    }
}

fn simple_module(name: &str, functions: Vec<Function>) -> Module {
    Module {
        name: name.into(),
        functions,
        interfaces: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }
}

/// Build the model (test-only; the driver builds it in production) and
/// render with the default naming (module-prefix stripping on).
fn render(api: &Api, module_name: &str, prefix: &str) -> String {
    let model = BindingModel::build(api, prefix);
    render_ruby_module(&model, module_name, true, "weaveffi.rb", "weaveffi.yml")
}

/// A function literal with the boilerplate zeroed; tests override the
/// fields they exercise.
fn plain_fn(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
    Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
        since: None,
    }
}

fn str_param(name: &str) -> Param {
    Param {
        name: name.into(),
        ty: TypeRef::StringUtf8,
        mutable: false,
        doc: None,
    }
}

/// A `kv` module with a declared error domain, an interface with a `new`
/// constructor, a factory constructor, methods (sync throwing, sync
/// non-throwing, async), a static, plus throwing and non-throwing free
/// functions.
fn kv_api() -> Api {
    let mut m = simple_module(
        "kv",
        vec![
            {
                let mut f = plain_fn(
                    "kv_lookup",
                    vec![str_param("key")],
                    Some(TypeRef::StringUtf8),
                );
                f.throws = true;
                f
            },
            plain_fn("kv_ping", vec![], Some(TypeRef::Bool)),
        ],
    );
    m.errors = Some(ErrorDomain {
        name: "KvError".into(),
        codes: vec![
            ErrorCode {
                name: "KeyNotFound".into(),
                code: 1001,
                message: "key not found".into(),
                doc: Some("Raised when the key is absent.".into()),
                fields: vec![],
            },
            ErrorCode {
                name: "IoError".into(),
                code: 1004,
                message: "I/O failure".into(),
                doc: None,
                fields: vec![],
            },
        ],
    });
    m.interfaces = vec![InterfaceDef {
        name: "Store".into(),
        doc: Some("A key-value store.".into()),
        constructors: vec![
            {
                let mut f = plain_fn("new", vec![str_param("path")], None);
                f.throws = true;
                f
            },
            {
                let mut f = plain_fn("open", vec![str_param("path")], None);
                f.throws = true;
                f
            },
        ],
        methods: vec![
            {
                let mut f = plain_fn("put", vec![str_param("key"), str_param("value")], None);
                f.throws = true;
                f
            },
            plain_fn("count", vec![], Some(TypeRef::U32)),
            {
                let mut f = plain_fn("compact", vec![], Some(TypeRef::Bool));
                f.r#async = true;
                f.cancellable = true;
                f.throws = true;
                f
            },
        ],
        statics: vec![plain_fn("default_capacity", vec![], Some(TypeRef::U32))],
    }];
    make_api(vec![m])
}

#[test]
fn name_returns_ruby() {
    assert_eq!(Generator::name(&RubyGenerator), "ruby");
}

#[test]
fn interface_ffi_attaches_destroy_and_members() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    assert!(
        code.contains("attach_function :weaveffi_kv_Store_destroy, [:pointer], :void"),
        "destroy attach: {code}"
    );
    assert!(
        code.contains("attach_function :weaveffi_kv_Store_new, [:string, :pointer], :pointer"),
        "ctor attach: {code}"
    );
    assert!(
        code.contains(
            "attach_function :weaveffi_kv_Store_put, [:pointer, :string, :string, :pointer], :void"
        ),
        "method attach includes self slot: {code}"
    );
    assert!(
        code.contains("attach_function :weaveffi_kv_Store_default_capacity, [:pointer], :uint32"),
        "static attach has no self slot: {code}"
    );
}

#[test]
fn interface_class_wraps_pointer_with_auto_pointer() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    assert!(
        code.contains("class StorePtr < FFI::AutoPointer"),
        "AutoPointer subclass: {code}"
    );
    assert!(
        code.contains("WeaveFFI.weaveffi_kv_Store_destroy(ptr)"),
        "release calls destroy symbol: {code}"
    );
    assert!(code.contains("def destroy"), "explicit destroy: {code}");
    assert!(code.contains("@handle.free"), "destroy frees: {code}");
    assert!(
        code.contains("def self._from_ptr(ptr)") && code.contains("obj = allocate"),
        "_from_ptr avoids initialize: {code}"
    );
}

#[test]
fn interface_new_ctor_maps_to_initialize() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    assert!(code.contains("def initialize(path)"), "initialize: {code}");
    assert!(
        code.contains("result = WeaveFFI.weaveffi_kv_Store_new(path, err)"),
        "ctor call: {code}"
    );
    assert!(
        code.contains("@handle = StorePtr.new(result)"),
        "handle assignment: {code}"
    );
}

#[test]
fn interface_named_ctor_is_class_method_factory() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    assert!(code.contains("def self.open(path)"), "factory def: {code}");
    assert!(
        code.contains("result = WeaveFFI.weaveffi_kv_Store_open(path, err)"),
        "factory call: {code}"
    );
    assert!(
        code.contains("_from_ptr(result)"),
        "factory wraps without initialize: {code}"
    );
}

#[test]
fn interface_method_passes_handle_first() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    assert!(code.contains("def put(key, value)"), "method def: {code}");
    assert!(
        code.contains("WeaveFFI.weaveffi_kv_Store_put(@handle, key, value, err)"),
        "self slot leads: {code}"
    );
}

#[test]
fn interface_static_is_class_method() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    assert!(
        code.contains("def self.default_capacity()"),
        "static def: {code}"
    );
    assert!(
        code.contains("result = WeaveFFI.weaveffi_kv_Store_default_capacity(err)"),
        "static call has no self slot: {code}"
    );
}

#[test]
fn typed_error_classes_and_helpers() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    assert!(code.contains("class KvError < Error"), "domain: {code}");
    assert!(
        code.contains("class KeyNotFound < KvError"),
        "code subclass: {code}"
    );
    assert!(code.contains("CODE = 1001"), "code constant: {code}");
    assert!(
        code.contains("def initialize(message = 'key not found')"),
        "default message: {code}"
    );
    assert!(
        code.contains("1004 => KvError::IoError,"),
        "code table: {code}"
    );
    assert!(
        code.contains("def self.kv_error_from(code, message, payload = nil)"),
        "factory helper: {code}"
    );
    assert!(
        code.contains("def self.check_kv_error!(err)"),
        "checker helper: {code}"
    );
    assert!(
        code.contains("raise kv_error_from(code, msg, payload)"),
        "checker raises typed: {code}"
    );
    assert!(
        code.contains(
            "payload = payload_ptr.null? ? nil : payload_ptr.read_string(err[:payload_len])"
        ),
        "checker copies payload before clearing: {code}"
    );
}

#[test]
fn error_payload_fields_decode_into_attributes() {
    let mut m = simple_module("kv", {
        let mut f = plain_fn("kv_load", vec![str_param("key")], None);
        f.throws = true;
        vec![f]
    });
    m.errors = Some(ErrorDomain {
        name: "KvError".into(),
        codes: vec![ErrorCode {
            name: "KeyNotFound".into(),
            code: 1001,
            message: "key not found".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "key".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "attempts".into(),
                    ty: TypeRef::U32,
                    doc: None,
                    default: None,
                },
            ],
        }],
    });
    let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
    // The exception class exposes the payload fields as attributes.
    assert!(
        code.contains("class KeyNotFound < KvError"),
        "code subclass: {code}"
    );
    assert!(
        code.contains("attr_reader :key") && code.contains("attr_reader :attempts"),
        "payload attrs: {code}"
    );
    assert!(
        code.contains("def initialize(message = 'key not found', key: nil, attempts: nil)"),
        "kwargs initialize: {code}"
    );
    // The factory decodes the value-buffer payload in declaration order.
    assert!(code.contains("when 1001"), "payload dispatch: {code}");
    assert!(
        code.contains("r = WvBufferReader.new(payload || ''.b)"),
        "payload reader: {code}"
    );
    assert!(
        code.contains("_wv_key = r.read_string") && code.contains("_wv_attempts = r.read_u32"),
        "field decode: {code}"
    );
    assert!(
        code.contains("KvError::KeyNotFound.new(message, key: _wv_key, attempts: _wv_attempts)"),
        "typed construction: {code}"
    );
}

#[test]
fn throwing_function_uses_typed_checker() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    let lookup = code
        .split("def self.kv_lookup(key)")
        .nth(1)
        .expect("kv_lookup wrapper");
    assert!(
        lookup.contains("check_kv_error!(err)"),
        "typed checker: {code}"
    );
}

#[test]
fn non_throwing_function_uses_generic_checker() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    let ping = code
        .split("def self.kv_ping()")
        .nth(1)
        .expect("kv_ping wrapper");
    let body = ping.split("\n  end").next().expect("wrapper body");
    assert!(body.contains("check_error!(err)"), "generic: {code}");
    assert!(
        !body.contains("check_kv_error!"),
        "no typed checker: {code}"
    );
}

#[test]
fn non_throwing_method_uses_generic_checker() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    let count = code.split("def count()").nth(1).expect("count wrapper");
    let body = count.split("\n    end").next().expect("method body");
    assert!(
        body.contains("WeaveFFI.check_error!(err)"),
        "generic qualified: {code}"
    );
    assert!(
        !body.contains("check_kv_error!"),
        "no typed checker: {code}"
    );
}

#[test]
fn async_member_routes_typed_error_and_self_slot() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    let compact = code.split("def compact()").nth(1).expect("compact wrapper");
    assert!(
        compact.contains("queue << WeaveFFI.kv_error_from(code, msg, payload)"),
        "typed async error: {code}"
    );
    assert!(
        compact.contains(
            "WeaveFFI.weaveffi_kv_Store_compact_async(@handle, FFI::Pointer::NULL, callback, FFI::Pointer::NULL)"
        ),
        "self slot then cancel token: {code}"
    );
}

#[test]
fn interface_params_borrow_and_returns_wrap() {
    let mut m = simple_module(
        "kv",
        vec![
            plain_fn(
                "clone_store",
                vec![Param {
                    name: "store".into(),
                    ty: TypeRef::Interface("Store".into()),
                    mutable: false,
                    doc: None,
                }],
                Some(TypeRef::Interface("Store".into())),
            ),
            plain_fn(
                "find_store",
                vec![],
                Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                    "Store".into(),
                )))),
            ),
        ],
    );
    m.interfaces = vec![InterfaceDef {
        name: "Store".into(),
        doc: None,
        constructors: vec![plain_fn("new", vec![], None)],
        methods: vec![],
        statics: vec![],
    }];
    let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
    assert!(
        code.contains("weaveffi_kv_clone_store(store.handle, err)"),
        "param borrows handle: {code}"
    );
    assert!(
        code.contains("Store._from_ptr(result)"),
        "return wraps owned pointer: {code}"
    );
    let find = code
        .split("def self.find_store()")
        .nth(1)
        .expect("find_store wrapper");
    assert!(
        find.contains("return nil if result.null?"),
        "optional interface nil: {code}"
    );
}

#[test]
fn naming_strips_module_prefix_by_default() {
    let api = make_api(vec![simple_module(
        "kv",
        vec![plain_fn("open_store", vec![], None)],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(
        code.contains("def self.open_store()"),
        "stripped name: {code}"
    );
    assert!(
        !code.contains("def self.kv_open_store()"),
        "no prefixed wrapper: {code}"
    );
    // The C symbol stays fully qualified regardless of wrapper naming.
    assert!(
        code.contains("weaveffi_kv_open_store(err)"),
        "C symbol: {code}"
    );
}

#[test]
fn naming_knob_restores_prefixed_wrappers() {
    let api = make_api(vec![simple_module(
        "kv",
        vec![plain_fn("open_store", vec![], None)],
    )]);
    let model = BindingModel::build(&api, "weaveffi");
    let code = render_ruby_module(&model, "WeaveFFI", false, "weaveffi.rb", "weaveffi.yml");
    assert!(
        code.contains("def self.kv_open_store()"),
        "prefixed name: {code}"
    );
}

#[test]
fn throwing_iterator_uses_typed_checker() {
    let mut m = simple_module("kv", {
        let mut f = plain_fn(
            "scan",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
        );
        f.throws = true;
        vec![f]
    });
    m.errors = Some(ErrorDomain {
        name: "KvError".into(),
        codes: vec![ErrorCode {
            name: "IoError".into(),
            code: 1004,
            message: "I/O failure".into(),
            doc: None,
            fields: vec![],
        }],
    });
    let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
    let scan = code.split("def self.scan()").nth(1).expect("scan wrapper");
    assert!(
        scan.contains("check_kv_error!(err)"),
        "launch checker: {code}"
    );
    assert!(
        scan.contains("check_kv_error!(item_err)"),
        "next checker: {code}"
    );
}

#[test]
fn generates_output_file() {
    let api = make_api(vec![simple_module(
        "math",
        vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let dir = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(dir.path()).unwrap();
    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let file = out_dir.join("ruby/lib/weaveffi.rb");
    assert!(file.exists(), "weaveffi.rb should exist");
    let contents = std::fs::read_to_string(&file).unwrap();
    assert!(contents.contains("require 'ffi'"));
    assert!(contents.contains("module WeaveFFI"));
    assert!(contents.contains("attach_function :weaveffi_math_add"));
    assert!(contents.contains("def self.add(a, b)"));
}

#[test]
fn output_files_returns_correct_path() {
    let api = make_api(vec![]);
    let out_dir = Utf8Path::new("/tmp/out");
    let files = RubyGenerator.output_files(&api, out_dir, &RubyConfig::default());
    assert_eq!(
        files,
        vec![
            format!("{out_dir}/ruby/README.md"),
            format!("{out_dir}/ruby/lib/weaveffi.rb"),
            format!("{out_dir}/ruby/weaveffi.gemspec"),
        ]
    );
}

#[test]
fn ruby_generates_gemspec() {
    let api = make_api(vec![simple_module("math", vec![])]);
    let dir = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(dir.path()).unwrap();
    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let gemspec = out_dir.join("ruby/weaveffi.gemspec");
    assert!(gemspec.exists(), "gemspec should exist");
    let contents = std::fs::read_to_string(&gemspec).unwrap();
    assert!(
        contents.contains("Gem::Specification.new do |s|"),
        "gemspec header: {contents}"
    );
    assert!(contents.contains("s.name"), "name field: {contents}");
    assert!(contents.contains("s.version"), "version field: {contents}");
    assert!(contents.contains("s.summary"), "summary field: {contents}");
    assert!(contents.contains("s.files"), "files field: {contents}");
    assert!(
        contents.contains("s.require_paths"),
        "require_paths: {contents}"
    );
    assert!(
        contents.contains("s.add_dependency 'ffi', '~> 1.15'"),
        "ffi dependency: {contents}"
    );

    let readme = out_dir.join("ruby/README.md");
    assert!(readme.exists(), "README should exist");
    let readme_contents = std::fs::read_to_string(&readme).unwrap();
    assert!(
        readme_contents.contains("gem build"),
        "usage instructions: {readme_contents}"
    );
}

#[test]
fn renders_enum_with_shouty_snake_case() {
    let api = make_api(vec![Module {
        name: "gfx".into(),
        functions: vec![],
        interfaces: vec![],
        structs: vec![],
        enums: vec![EnumDef {
            name: "Color".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Red".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "DarkBlue".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(code.contains("module Color"), "enum module: {code}");
    assert!(code.contains("RED = 0"), "RED: {code}");
    assert!(code.contains("DARK_BLUE = 1"), "DARK_BLUE: {code}");
}

#[test]
fn renders_struct_as_value_class() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![],
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(code.contains("class Contact"), "class: {code}");
    assert!(code.contains("attr_reader :id"), "id attr: {code}");
    assert!(code.contains("attr_reader :name"), "name attr: {code}");
    assert!(
        code.contains("def initialize(id:, name:)"),
        "kwargs initialize: {code}"
    );
    assert!(code.contains("def ==(other)"), "structural eq: {code}");
    assert!(
        code.contains("return false unless other.is_a?(Contact)"),
        "eq type guard: {code}"
    );
    // A record is a value type: no FFI pointer wrapping, no destroy, no
    // create, and no C symbols at all.
    assert!(
        !code.contains("ContactPtr") && !code.contains("FFI::AutoPointer"),
        "no pointer wrapper: {code}"
    );
    assert!(
        !code.contains("weaveffi_contacts_Contact_destroy"),
        "no destroy symbol: {code}"
    );
    assert!(
        !code.contains("attach_function :weaveffi_contacts_Contact"),
        "no record C symbols: {code}"
    );
}

#[test]
fn struct_codec_packs_and_unpacks_fields_in_order() {
    let api = make_api(vec![Module {
        name: "geo".into(),
        functions: vec![],
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Point".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "x".into(),
                    ty: TypeRef::F64,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "label".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // One private pack/unpack pair per record, fields in wire order.
    assert!(
        code.contains("def self._wv_write_point(w, v)"),
        "pack helper: {code}"
    );
    let pack = code
        .split("def self._wv_write_point(w, v)")
        .nth(1)
        .expect("pack body");
    let x_write = pack.find("w.write_f64(v.x)").expect("x write");
    let label_write = pack.find("w.write_string(v.label)").expect("label write");
    assert!(x_write < label_write, "declaration order: {code}");
    assert!(
        code.contains("def self._wv_read_point(r)"),
        "unpack helper: {code}"
    );
    assert!(
        code.contains("_wv_x = r.read_f64") && code.contains("_wv_label = r.read_string"),
        "field reads: {code}"
    );
    assert!(
        code.contains("Point.new(x: _wv_x, label: _wv_label)"),
        "unpack constructs value class: {code}"
    );
    // Builders are gone entirely.
    assert!(!code.contains("PointBuilder"), "no builder class: {code}");
    assert!(
        !code.contains("weaveffi_geo_Point_create"),
        "no create symbol: {code}"
    );
}

#[test]
fn function_wrapper_checks_error() {
    let api = make_api(vec![simple_module(
        "math",
        vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(code.contains("err = ErrorStruct.new"), "err alloc: {code}");
    assert!(code.contains("check_error!(err)"), "check_error: {code}");
}

#[test]
fn string_return_reads_and_frees() {
    let api = make_api(vec![simple_module(
        "data",
        vec![Function {
            name: "get_name".into(),
            params: vec![],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(code.contains("result.read_string"), "read_string: {code}");
    assert!(
        code.contains("weaveffi_free_string(result)"),
        "free_string: {code}"
    );
    assert!(
        code.contains("return '' if result.null?"),
        "null check: {code}"
    );
}

#[test]
fn bool_param_and_return_conversion() {
    let api = make_api(vec![simple_module(
        "check",
        vec![Function {
            name: "is_valid".into(),
            params: vec![Param {
                name: "value".into(),
                ty: TypeRef::Bool,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(
        code.contains("value_c = value ? 1 : 0"),
        "bool param: {code}"
    );
    assert!(code.contains("result != 0"), "bool return: {code}");
}

#[test]
fn optional_string_returns_nil() {
    let api = make_api(vec![simple_module(
        "data",
        vec![Function {
            name: "find".into(),
            params: vec![],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // An optional string is buffered: a flag byte selects nil or the value.
    assert!(code.contains("if _wv_r.read_flag"), "flag byte: {code}");
    assert!(
        code.contains("_wv_value = _wv_r.read_string"),
        "present decode: {code}"
    );
    assert!(code.contains("_wv_value = nil"), "absent is nil: {code}");
    assert!(
        code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
        "returned buffer freed: {code}"
    );
}

#[test]
fn list_return_uses_array() {
    let api = make_api(vec![simple_module(
        "data",
        vec![Function {
            name: "list_ids".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // A list return is one value buffer: count prefix, then elements.
    assert!(
        code.contains("_wv_value = Array.new(_wv_r.read_len) do"),
        "count-driven array: {code}"
    );
    assert!(
        code.contains("_wv_e0 = _wv_r.read_i32"),
        "element decode: {code}"
    );
    assert!(
        code.contains("_wv_r.expect_end!"),
        "trailing bytes rejected: {code}"
    );
}

#[test]
fn map_return_builds_hash() {
    let api = make_api(vec![simple_module(
        "data",
        vec![Function {
            name: "get_metadata".into(),
            params: vec![],
            returns: Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // A map return is one value buffer: count, then alternating key/value.
    assert!(code.contains("_wv_value = {}"), "hash init: {code}");
    assert!(
        code.contains("_wv_r.read_len.times do"),
        "count-driven loop: {code}"
    );
    assert!(
        code.contains("_wv_k0 = _wv_r.read_string") && code.contains("_wv_v0 = _wv_r.read_i32"),
        "key/value decode: {code}"
    );
    assert!(
        code.contains("_wv_value[_wv_k0] = _wv_v0"),
        "hash insert: {code}"
    );
    // No parallel-array ABI remains.
    assert!(!code.contains("out_keys"), "no out_keys: {code}");
    assert!(!code.contains("out_values"), "no out_values: {code}");
}

#[test]
fn list_of_strings_return_frees_elements_and_buffer() {
    let api = make_api(vec![simple_module(
        "data",
        vec![plain_fn(
            "list_names",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
        )],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // String elements are decoded from the single value buffer (copies),
    // and only that buffer itself is released.
    assert!(
        code.contains("_wv_e0 = _wv_r.read_string"),
        "string elements decoded: {code}"
    );
    assert!(
        code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
        "value buffer freed: {code}"
    );
    assert!(
        !code.contains("weaveffi_free_string("),
        "no per-element frees remain: {code}"
    );
}

#[test]
fn scalar_list_return_frees_buffer() {
    let api = make_api(vec![simple_module(
        "data",
        vec![plain_fn(
            "list_ids",
            vec![],
            Some(TypeRef::List(Box::new(TypeRef::I32))),
        )],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(
        code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
        "value buffer freed: {code}"
    );
}

#[test]
fn map_return_decodes_from_one_buffer() {
    let api = make_api(vec![simple_module(
        "data",
        vec![plain_fn(
            "get_metadata",
            vec![],
            Some(TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32),
            )),
        )],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // Keys and values decode from the single buffer; only it is freed.
    assert!(
        code.contains("_wv_k0 = _wv_r.read_string"),
        "key decode: {code}"
    );
    assert!(
        code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
        "value buffer freed: {code}"
    );
    assert!(
        !code.contains("keys_ptr") && !code.contains("vals_ptr"),
        "no parallel buffers remain: {code}"
    );
}

#[test]
fn optional_scalar_return_decodes_flag_byte() {
    let api = make_api(vec![simple_module(
        "data",
        vec![plain_fn(
            "find_count",
            vec![],
            Some(TypeRef::Optional(Box::new(TypeRef::I32))),
        )],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // An optional scalar is buffered: flag byte, then the value.
    assert!(code.contains("if _wv_r.read_flag"), "flag byte: {code}");
    assert!(
        code.contains("_wv_value = _wv_r.read_i32"),
        "present decode: {code}"
    );
    assert!(code.contains("_wv_value = nil"), "absent is nil: {code}");
    assert!(
        code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
        "value buffer freed: {code}"
    );
}

#[test]
fn struct_return_wraps_in_class() {
    let api = make_api(vec![Module {
        name: "data".into(),
        functions: vec![Function {
            name: "get_item".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I64,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Item".into())),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Item".into(),
            doc: None,
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: None,
                default: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // A record return is decoded from its value buffer, which is then
    // released with the runtime's free_bytes.
    assert!(
        code.contains("out_len = FFI::MemoryPointer.new(:size_t)"),
        "out_len allocated: {code}"
    );
    assert!(
        code.contains("_wv_value = _wv_read_item(_wv_r)"),
        "record decode: {code}"
    );
    assert!(
        code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
        "value buffer freed: {code}"
    );
}

#[test]
fn async_function_generates_blocking_wrapper() {
    let api = make_api(vec![simple_module(
        "io",
        vec![Function {
            name: "read".into(),
            params: vec![],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // Completion callback type + launcher attach.
    assert!(
        code.contains("callback :weaveffi_io_read_callback, [:pointer, :pointer, :pointer], :void"),
        "async callback decl: {code}"
    );
    assert!(
        code.contains(
            "attach_function :weaveffi_io_read_async, [:weaveffi_io_read_callback, :pointer], :void"
        ),
        "async launcher attach: {code}"
    );
    // Blocking wrapper: trampoline pinned in a local, Queue rendezvous,
    // error re-raised on the caller thread.
    assert!(code.contains("def self.read()"), "wrapper: {code}");
    assert!(code.contains("queue = Queue.new"), "queue: {code}");
    assert!(
        code.contains("callback = FFI::Function.new(:void, [:pointer, :pointer, :pointer])"),
        "trampoline: {code}"
    );
    assert!(
        code.contains("weaveffi_io_read_async(callback, FFI::Pointer::NULL)"),
        "launch call: {code}"
    );
    assert!(code.contains("value = queue.pop"), "blocking pop: {code}");
    assert!(
        code.contains("raise value if value.is_a?(Error)"),
        "error re-raise: {code}"
    );
    // The generated doc states plainly that the call blocks.
    assert!(
        code.contains("# Blocks the current thread until the async producer completes"),
        "blocking doc: {code}"
    );
    // The completion callback copies the borrowed result buffer and must
    // not free it: the producer owns callback result buffers.
    assert!(
        code.contains("result.read_string"),
        "result copied in callback: {code}"
    );
    assert!(
        !code.contains("weaveffi_free_string(result)"),
        "borrowed callback buffer must not be freed: {code}"
    );
}

#[test]
fn async_bytes_result_copied_not_freed() {
    let api = make_api(vec![simple_module(
        "io",
        vec![Function {
            name: "fetch".into(),
            params: vec![],
            returns: Some(TypeRef::Bytes),
            doc: None,
            throws: false,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(
        code.contains("result.read_string(result_len)"),
        "bytes copied in callback: {code}"
    );
    assert!(
        !code.contains("weaveffi_free_bytes(result, result_len)"),
        "borrowed callback bytes must not be freed: {code}"
    );
}

#[test]
fn iterator_uses_next_destroy_protocol() {
    let api = make_api(vec![simple_module(
        "events",
        vec![Function {
            name: "get_messages".into(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // Launch returns the opaque iterator; next/destroy attached.
    assert!(
        code.contains("attach_function :weaveffi_events_get_messages, [:pointer], :pointer"),
        "launch attach: {code}"
    );
    assert!(
        code.contains(
            "attach_function :weaveffi_events_GetMessagesIterator_next, [:pointer, :pointer, :pointer], :int32"
        ),
        "next attach: {code}"
    );
    assert!(
        code.contains(
            "attach_function :weaveffi_events_GetMessagesIterator_destroy, [:pointer], :void"
        ),
        "destroy attach: {code}"
    );
    // The wrapper pulls via the iterator protocol, not the list ABI
    // (the old lowering wrongly passed an out_len the symbol lacks).
    assert!(
        code.contains(
            "has_item = weaveffi_events_GetMessagesIterator_next(iter, out_item, item_err)"
        ),
        "pull loop: {code}"
    );
    assert!(
        code.contains("weaveffi_events_GetMessagesIterator_destroy(iter) unless iter.null?"),
        "destroy on disposal: {code}"
    );
    assert!(!code.contains("out_len"), "no stray out_len: {code}");
}

#[test]
fn iterator_returns_lazy_enumerator_with_ensured_destroy() {
    let api = make_api(vec![simple_module(
        "events",
        vec![Function {
            name: "get_messages".into(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    let body = code
        .split("def self.get_messages()")
        .nth(1)
        .expect("wrapper body");
    let body = body.split("\n  end\n").next().expect("wrapper body end");
    // Lazy Enumerator, never a hidden drain into an Array.
    assert!(
        body.contains("Enumerator.new do |y|"),
        "lazy Enumerator: {code}"
    );
    assert!(!body.contains("items = []"), "no eager drain: {code}");
    assert!(!body.contains(".to_a"), "no hidden collect: {code}");
    // The launch happens inside the block, so an unstarted enumerator
    // never acquires (and thus can never leak) a handle.
    let launch = body
        .find("iter = weaveffi_events_get_messages(err)")
        .expect("launch");
    let enum_open = body.find("Enumerator.new do |y|").expect("enumerator");
    assert!(enum_open < launch, "launch inside enumerator block: {code}");
    // Destroy runs from an ensure block, guarding early break, and each
    // yielded string is freed after copying.
    let ensure_pos = body.find("ensure").expect("ensure block");
    let destroy_pos = body
        .find("weaveffi_events_GetMessagesIterator_destroy(iter)")
        .expect("destroy call");
    assert!(ensure_pos < destroy_pos, "destroy inside ensure: {code}");
    assert!(
        body.contains("weaveffi_free_string(item_ptr)"),
        "yielded string freed after copy: {code}"
    );
    assert!(body.contains("y << item"), "yields through yielder: {code}");
    // The generated docs describe the lazy contract.
    assert!(
        code.contains("# Returns a lazy Enumerator"),
        "doc states Enumerator return: {code}"
    );
}

#[test]
fn iterator_of_records_adopts_each_element() {
    let api = make_api(vec![Module {
        name: "kv".into(),
        functions: vec![Function {
            name: "scan_entries".into(),
            params: vec![],
            returns: Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Entry".into(),
            doc: None,
            fields: vec![StructField {
                name: "key".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // Each yielded record arrives as a producer-allocated value buffer:
    // the wrapper copies the bytes, frees them, then decodes and yields.
    assert!(
        code.contains("out_item_len = FFI::MemoryPointer.new(:size_t)"),
        "element length out-param: {code}"
    );
    assert!(
        code.contains("weaveffi_free_bytes(item_ptr, item_len) unless item_ptr.null?"),
        "element buffer freed: {code}"
    );
    assert!(
        code.contains("_wv_item = _wv_read_entry(_wv_r)"),
        "element decoded: {code}"
    );
    assert!(code.contains("y << _wv_item"), "decoded yield: {code}");
    assert!(
        code.contains("Enumerator.new do |y|"),
        "record iterator is lazy: {code}"
    );
}

#[test]
fn interface_iterator_method_is_lazy_and_qualified() {
    let mut m = simple_module("kv", vec![]);
    m.interfaces = vec![InterfaceDef {
        name: "Store".into(),
        doc: None,
        constructors: vec![plain_fn("new", vec![], None)],
        methods: vec![plain_fn(
            "keys",
            vec![],
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
        )],
        statics: vec![],
    }];
    let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
    let body = code.split("def keys()").nth(1).expect("keys wrapper");
    assert!(
        body.contains("Enumerator.new do |y|"),
        "method iterator is lazy: {code}"
    );
    assert!(
        body.contains("iter = WeaveFFI.weaveffi_kv_Store_keys(@handle, err)"),
        "launch passes self and qualifies: {code}"
    );
    assert!(
        body.contains("WeaveFFI.weaveffi_kv_Store_KeysIterator_destroy(iter) unless iter.null?"),
        "qualified ensure destroy: {code}"
    );
}

#[test]
fn listener_register_unregister_wrappers() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        callbacks: vec![CallbackDef {
            name: "OnMessage".into(),
            params: vec![Param {
                name: "message".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "message_listener".into(),
            event_callback: "OnMessage".into(),
            doc: None,
        }],
        ..simple_module("events", vec![])
    }]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(
        code.contains("callback :weaveffi_events_OnMessage_fn, [:string, :pointer], :void"),
        "callback decl: {code}"
    );
    assert!(
        code.contains(
            "attach_function :weaveffi_events_register_message_listener, [:weaveffi_events_OnMessage_fn, :pointer], :uint64"
        ),
        "register attach: {code}"
    );
    assert!(
        code.contains("def self.register_message_listener(&block)"),
        "register wrapper: {code}"
    );
    assert!(
        code.contains("@listener_refs[listener_id] = trampoline"),
        "trampoline pinned: {code}"
    );
    assert!(
        code.contains("def self.unregister_message_listener(listener_id)"),
        "unregister wrapper: {code}"
    );
    assert!(
        code.contains("@listener_refs.delete(listener_id)"),
        "trampoline released: {code}"
    );
}

#[test]
fn preamble_has_platform_detection() {
    let code = render(&make_api(vec![]), "WeaveFFI", "weaveffi");
    assert!(code.contains("FFI::Platform::OS"), "platform: {code}");
    assert!(code.contains("libweaveffi.dylib"), "darwin: {code}");
    assert!(code.contains("weaveffi.dll"), "windows: {code}");
    assert!(code.contains("libweaveffi.so"), "linux: {code}");
}

#[test]
fn error_class_structure() {
    let code = render(&make_api(vec![]), "WeaveFFI", "weaveffi");
    assert!(
        code.contains("class Error < StandardError"),
        "Error class: {code}"
    );
    assert!(code.contains("attr_reader :code"), "code attr: {code}");
    // The error struct layout carries the structured payload slots.
    assert!(
        code.contains(":payload_ptr, :pointer") && code.contains(":payload_len, :size_t"),
        "payload slots in ErrorStruct: {code}"
    );
}

#[test]
fn preamble_has_buffer_runtime() {
    let code = render(&make_api(vec![]), "WeaveFFI", "weaveffi");
    assert!(
        code.contains("class WvBufferWriter"),
        "buffer writer: {code}"
    );
    assert!(
        code.contains("class WvBufferReader"),
        "buffer reader: {code}"
    );
    // Little-endian packed directives and strict decoding guards.
    assert!(code.contains("[v].pack('l<')"), "LE i32 pack: {code}");
    assert!(code.contains("unpack1('E')"), "f64 unpack: {code}");
    assert!(
        code.contains("'malformed value buffer: trailing bytes after value'"),
        "trailing byte guard: {code}"
    );
    assert!(
        code.contains("'malformed value buffer: string is not valid UTF-8'"),
        "UTF-8 guard: {code}"
    );
    assert!(
        code.contains("'malformed value buffer: length prefix exceeds remaining bytes'"),
        "length guard: {code}"
    );
}

#[test]
fn handle_type_uses_uint64() {
    let api = make_api(vec![simple_module(
        "store",
        vec![Function {
            name: "create".into(),
            params: vec![],
            returns: Some(TypeRef::Handle),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(code.contains(":uint64"), "handle type: {code}");
}

#[test]
fn ffi_type_mapping() {
    let types = |ty: &TypeRef| rb_abi_types(&abi::lower_param("_", ty, "", false), false);
    assert_eq!(types(&TypeRef::I32), vec![":int32"]);
    assert_eq!(types(&TypeRef::U32), vec![":uint32"]);
    assert_eq!(types(&TypeRef::I64), vec![":int64"]);
    assert_eq!(types(&TypeRef::F64), vec![":double"]);
    assert_eq!(types(&TypeRef::Bool), vec![":int32"]);
    assert_eq!(types(&TypeRef::Handle), vec![":uint64"]);
    assert_eq!(types(&TypeRef::StringUtf8), vec![":string"]);
    assert_eq!(types(&TypeRef::Enum("Color".into())), vec![":int32"]);
    // Buffered types lower to a (ptr, len) slot pair.
    assert_eq!(
        types(&TypeRef::Record("Foo".into())),
        vec![":pointer", ":size_t"]
    );
    assert_eq!(
        types(&TypeRef::List(Box::new(TypeRef::I32))),
        vec![":pointer", ":size_t"]
    );
}

#[test]
fn return_type_string_is_pointer() {
    let ret = abi::lower_return(&TypeRef::StringUtf8, "");
    assert_eq!(rb_ffi_type(&ret.ret, true), ":pointer");
}

#[test]
fn return_type_map_is_buffer_with_out_len() {
    let ret = abi::lower_return(
        &TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
        "",
    );
    assert_eq!(rb_ffi_type(&ret.ret, true), ":pointer");
    let out: Vec<_> = ret
        .out_params
        .iter()
        .map(|p| rb_ffi_type(&p.ty, true))
        .collect();
    assert_eq!(out, vec![":pointer"]);
}

#[test]
fn enum_param_passes_int32() {
    let api = make_api(vec![simple_module(
        "gfx",
        vec![Function {
            name: "set_color".into(),
            params: vec![Param {
                name: "color".into(),
                ty: TypeRef::Enum("Color".into()),
                mutable: false,
                doc: None,
            }],
            returns: None,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(code.contains(":int32"), "enum type: {code}");
}

#[test]
fn void_function_no_result() {
    let api = make_api(vec![simple_module(
        "store",
        vec![Function {
            name: "clear".into(),
            params: vec![],
            returns: None,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    assert!(code.contains(":void"), "void return: {code}");
    assert!(
        !code.contains("result = weaveffi_store_clear"),
        "no result capture: {code}"
    );
}

#[test]
fn list_of_structs_return() {
    let api = make_api(vec![Module {
        name: "data".into(),
        functions: vec![Function {
            name: "list_items".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::Record("Item".into())))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Item".into(),
            doc: None,
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: None,
                default: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // Record list elements decode recursively through the record codec.
    assert!(
        code.contains("_wv_e0 = _wv_read_item(_wv_r)"),
        "struct list element: {code}"
    );
    assert!(
        code.contains("_wv_value = Array.new(_wv_r.read_len) do"),
        "count-driven array: {code}"
    );
}

#[test]
fn optional_struct_returns_nil_on_null() {
    let api = make_api(vec![simple_module(
        "data",
        vec![Function {
            name: "find_item".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I64,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::Record("Item".into())))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "weaveffi");
    // An optional record is buffered: the flag byte selects nil or a
    // decoded value class instance.
    assert!(code.contains("if _wv_r.read_flag"), "flag byte: {code}");
    assert!(
        code.contains("_wv_value = _wv_read_item(_wv_r)"),
        "present decode: {code}"
    );
    assert!(code.contains("_wv_value = nil"), "absent is nil: {code}");
}

// ── Comprehensive tests ──

fn contacts_api() -> Api {
    Api {
        version: "0.6.0".into(),
        modules: vec![Module {
            name: "contacts".into(),
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Record("Contact".into())),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "delete_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::Bool),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "count_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![EnumDef {
                name: "ContactType".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Personal".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Work".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Other".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }],
        generators: None,
        package: None,
    }
}

#[test]
fn generate_ruby_basic() {
    let api = make_api(vec![simple_module(
        "math",
        vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
    assert!(rb.contains("module WeaveFFI"), "module name: {rb}");
    assert!(
        rb.contains("attach_function :weaveffi_math_add"),
        "attach_function: {rb}"
    );
    assert!(rb.contains("def self.add(a, b)"), "wrapper fn: {rb}");
    assert!(rb.contains("check_error!(err)"), "error check: {rb}");
}

#[test]
fn generate_ruby_with_structs() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "get_contact".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::Handle,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "first_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "last_name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                },
            ],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
    assert!(rb.contains("class Contact"), "struct class: {rb}");
    assert!(
        rb.contains("attr_reader :first_name"),
        "first_name attr: {rb}"
    );
    assert!(
        rb.contains("attr_reader :last_name"),
        "last_name attr: {rb}"
    );
    assert!(
        rb.contains("def initialize(first_name:, last_name:)"),
        "kwargs initialize: {rb}"
    );
    assert!(
        rb.contains("_wv_value = _wv_read_contact(_wv_r)"),
        "struct return decode: {rb}"
    );
    assert!(
        !rb.contains("FFI::AutoPointer"),
        "no pointer wrapping remains: {rb}"
    );
}

#[test]
fn generate_ruby_with_enums() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "classify".into(),
            params: vec![Param {
                name: "ct".into(),
                ty: TypeRef::Enum("ContactType".into()),
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Enum("ContactType".into())),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        interfaces: vec![],
        structs: vec![],
        enums: vec![EnumDef {
            name: "ContactType".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Personal".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Work".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Other".into(),
                    value: 2,
                    doc: None,
                    fields: vec![],
                },
            ],
        }],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
    assert!(rb.contains("module ContactType"), "enum module: {rb}");
    assert!(rb.contains("PERSONAL = 0"), "variant 0: {rb}");
    assert!(rb.contains("WORK = 1"), "variant 1: {rb}");
    assert!(rb.contains("OTHER = 2"), "variant 2: {rb}");
    assert!(rb.contains(":int32"), "enum ffi type: {rb}");
}

#[test]
fn generate_ruby_with_optionals() {
    let api = make_api(vec![simple_module(
        "data",
        vec![
            Function {
                name: "find_name".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "find_count".into(),
                params: vec![Param {
                    name: "key".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
    // Optional returns decode a flag byte from the value buffer.
    assert!(
        rb.contains("if _wv_r.read_flag"),
        "flag byte on optional return: {rb}"
    );
    assert!(rb.contains("_wv_value = nil"), "absent is nil: {rb}");
    // An optional parameter packs a flag byte (plus the value when
    // present) into the value buffer handed to the C call.
    assert!(
        rb.contains("key_w = WvBufferWriter.new"),
        "optional param writer: {rb}"
    );
    assert!(
        rb.contains("key_w.write_flag(false)") && rb.contains("key_w.write_flag(true)"),
        "optional param flag: {rb}"
    );
    assert!(
        rb.contains("key_w.write_i32(key)"),
        "optional param value: {rb}"
    );
    assert!(
        rb.contains("key_buf, key_data.bytesize"),
        "optional param slot pair: {rb}"
    );
}

#[test]
fn generate_ruby_with_lists() {
    let api = make_api(vec![simple_module(
        "data",
        vec![
            Function {
                name: "list_ids".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "set_names".into(),
                params: vec![Param {
                    name: "names".into(),
                    ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
    // List returns decode count-prefixed elements from the value buffer.
    assert!(
        rb.contains("_wv_value = Array.new(_wv_r.read_len) do"),
        "list return decode: {rb}"
    );
    // A list parameter packs count then elements, and hands the C call a
    // MemoryPointer copy of the encoding.
    assert!(
        rb.contains("names_w.write_len(names.length)"),
        "list param count: {rb}"
    );
    assert!(
        rb.contains("names.each do |_wv_e0|"),
        "list param elements: {rb}"
    );
    assert!(
        rb.contains("names_w.write_string(_wv_e0)"),
        "list param element write: {rb}"
    );
    assert!(
        rb.contains("names_buf = FFI::MemoryPointer.new(:uint8, names_data.bytesize)"),
        "list param buffer copy: {rb}"
    );
    assert!(
        rb.contains("names_buf, names_data.bytesize"),
        "list param slot pair: {rb}"
    );
}

#[test]
fn generate_ruby_full_contacts() {
    let api = contacts_api();

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();

    let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();

    assert!(rb.contains("module WeaveFFI"), "module: {rb}");
    assert!(rb.contains("module ContactType"), "enum: {rb}");
    assert!(rb.contains("PERSONAL = 0"), "enum variant: {rb}");
    assert!(rb.contains("class Contact"), "struct class: {rb}");
    assert!(
        rb.contains("def self.create_contact(first_name, last_name, email, contact_type)"),
        "create fn: {rb}"
    );
    assert!(rb.contains("def self.get_contact(id)"), "get fn: {rb}");
    assert!(rb.contains("def self.list_contacts"), "list fn: {rb}");
    assert!(
        rb.contains("def self.delete_contact(id)"),
        "delete fn: {rb}"
    );
    assert!(rb.contains("def self.count_contacts"), "count fn: {rb}");
    assert!(rb.contains("attr_reader :id"), "id attr: {rb}");
    assert!(
        rb.contains("attr_reader :first_name"),
        "first_name attr: {rb}"
    );
    assert!(rb.contains("attr_reader :email"), "email attr: {rb}");
    assert!(
        rb.contains("attr_reader :contact_type"),
        "contact_type attr: {rb}"
    );

    let gemspec = std::fs::read_to_string(tmp.path().join("ruby/weaveffi.gemspec")).unwrap();
    assert!(
        gemspec.contains("s.name        = 'weaveffi'"),
        "gem name: {gemspec}"
    );

    let readme = std::fs::read_to_string(tmp.path().join("ruby/README.md")).unwrap();
    assert!(readme.contains("Ruby"), "readme: {readme}");
}

#[test]
fn ruby_custom_module_name() {
    let api = make_api(vec![simple_module(
        "math",
        vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let tmp = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

    let config = RubyConfig {
        module_name: Some("MyBindings".into()),
        gem_name: Some("my_bindings".into()),
        ..RubyConfig::default()
    };
    RubyGenerator.generate(&api, out_dir, &config).unwrap();

    let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/my_bindings.rb")).unwrap();
    assert!(rb.contains("module MyBindings"), "custom module name: {rb}");
    assert!(
        !rb.contains("module WeaveFFI"),
        "should not contain default module name: {rb}"
    );

    let gemspec = std::fs::read_to_string(tmp.path().join("ruby/my_bindings.gemspec")).unwrap();
    assert!(
        gemspec.contains("s.name        = 'my_bindings'"),
        "custom gem name: {gemspec}"
    );
    assert!(
        !gemspec.contains("s.name        = 'weaveffi'"),
        "should not contain default gem name: {gemspec}"
    );
}

#[test]
fn ruby_no_double_free_on_error() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Contact".into(),
            doc: None,
            fields: vec![StructField {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        functions: vec![Function {
            name: "find_contact".into(),
            params: vec![Param {
                name: "name".into(),
                ty: TypeRef::StringUtf8,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Record("Contact".into())),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        errors: None,
        modules: vec![],
    }]);

    let rb = render(&api, "WeaveFFI", "weaveffi");

    let fn_start = rb
        .find("def self.find_contact(name)")
        .expect("find_contact wrapper");
    let fn_body = &rb[fn_start..];
    let fn_end = fn_body.find("\n  end\n").unwrap();
    let fn_text = &fn_body[..fn_end];

    assert!(
        !fn_text.contains("weaveffi_free_string(name"),
        "borrowed string param must not be freed by wrapper: {fn_text}"
    );

    let err_check = fn_text
        .find("check_error!(err)")
        .expect("check_error in find_contact");
    let buffer_free = fn_text
        .find("weaveffi_free_bytes(result, len)")
        .expect("free_bytes in find_contact");
    let decode = fn_text
        .find("_wv_read_contact(_wv_r)")
        .expect("decode in find_contact");
    assert!(
        err_check < buffer_free,
        "error must be checked before touching the result buffer: {fn_text}"
    );
    assert!(
        buffer_free < decode,
        "buffer is copied and freed exactly once before decoding: {fn_text}"
    );
    assert_eq!(
        fn_text.matches("weaveffi_free_bytes(result").count(),
        1,
        "result buffer freed exactly once: {fn_text}"
    );
}

#[test]
fn ruby_null_check_on_optional_return() {
    let api = make_api(vec![Module {
        name: "contacts".into(),
        functions: vec![Function {
            name: "find_contact".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I64,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                "Contact".into(),
            )))),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        interfaces: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);

    let rb = render(&api, "WeaveFFI", "weaveffi");

    let fn_start = rb
        .find("def self.find_contact(id)")
        .expect("find_contact wrapper");
    let fn_body = &rb[fn_start..];
    let fn_end = fn_body.find("\n  end\n").unwrap();
    let fn_text = &fn_body[..fn_end];

    // The flag byte gates decoding: the record codec only runs for a
    // present value, and an absent one yields nil.
    let flag_check = fn_text
        .find("if _wv_r.read_flag")
        .expect("flag check in find_contact");
    let contact_decode = fn_text
        .find("_wv_read_contact(_wv_r)")
        .expect("decode in find_contact");
    assert!(
        flag_check < contact_decode,
        "optional record return should check the flag before decoding: {fn_text}"
    );
    assert!(
        fn_text.contains("_wv_value = nil"),
        "absent optional is nil: {fn_text}"
    );
}

fn doc_api() -> Api {
    make_api(vec![Module {
        name: "docs".into(),
        functions: vec![Function {
            name: "do_thing".into(),
            params: vec![Param {
                name: "x".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: Some("the input value".into()),
            }],
            returns: Some(TypeRef::I32),
            doc: Some("Performs a thing.".into()),
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
        interfaces: vec![],
        structs: vec![StructDef {
            name: "Item".into(),
            doc: Some("An item we track.".into()),
            fields: vec![StructField {
                name: "id".into(),
                ty: TypeRef::I64,
                doc: Some("Stable id".into()),
                default: None,
            }],
        }],
        enums: vec![EnumDef {
            name: "Kind".into(),
            doc: Some("Kind of item.".into()),
            variants: vec![EnumVariant {
                name: "Small".into(),
                value: 0,
                doc: Some("A small one".into()),
                fields: vec![],
            }],
        }],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }])
}

#[test]
fn ruby_emits_doc_on_function() {
    let rb = render(&doc_api(), "Weaveffi", "weaveffi");
    assert!(rb.contains("# Performs a thing."), "{rb}");
}

#[test]
fn ruby_emits_doc_on_struct() {
    let rb = render(&doc_api(), "Weaveffi", "weaveffi");
    assert!(rb.contains("# An item we track."), "{rb}");
}

#[test]
fn ruby_emits_doc_on_enum_variant() {
    let rb = render(&doc_api(), "Weaveffi", "weaveffi");
    assert!(rb.contains("# Kind of item."), "{rb}");
    assert!(rb.contains("# A small one"), "{rb}");
}

#[test]
fn ruby_emits_doc_on_field() {
    let rb = render(&doc_api(), "Weaveffi", "weaveffi");
    assert!(rb.contains("# Stable id"), "{rb}");
}

#[test]
fn ruby_emits_doc_on_param() {
    let rb = render(&doc_api(), "Weaveffi", "weaveffi");
    assert!(rb.contains("# @param x [Object] the input value"), "{rb}");
}

#[test]
fn ruby_custom_prefix_threads_to_user_symbols() {
    let api = make_api(vec![simple_module(
        "math",
        vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }],
    )]);

    let code = render(&api, "WeaveFFI", "myffi");

    assert!(
        code.contains("attach_function :myffi_math_add"),
        "user symbol should adopt custom prefix: {code}"
    );
    assert!(
        !code.contains("weaveffi_math_add"),
        "user symbol must not retain default prefix: {code}"
    );
    assert!(
        code.contains("weaveffi_error_clear"),
        "runtime ABI helper must stay literal: {code}"
    );
}

fn shapes_api() -> Api {
    make_api(vec![Module {
        name: "shapes".into(),
        functions: vec![
            Function {
                name: "describe".into(),
                params: vec![Param {
                    name: "shape".into(),
                    ty: TypeRef::RichEnum("Shape".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
            Function {
                name: "scale".into(),
                params: vec![
                    Param {
                        name: "shape".into(),
                        ty: TypeRef::RichEnum("Shape".into()),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "factor".into(),
                        ty: TypeRef::F64,
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::RichEnum("Shape".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ],
        interfaces: vec![],
        structs: vec![],
        enums: vec![
            EnumDef {
                name: "Shape".into(),
                doc: Some("An algebraic shape".into()),
                variants: vec![
                    EnumVariant {
                        name: "Empty".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Circle".into(),
                        value: 1,
                        doc: None,
                        fields: vec![StructField {
                            name: "radius".into(),
                            ty: TypeRef::F64,
                            doc: None,
                            default: None,
                        }],
                    },
                    EnumVariant {
                        name: "Rectangle".into(),
                        value: 2,
                        doc: None,
                        fields: vec![
                            StructField {
                                name: "width".into(),
                                ty: TypeRef::F32,
                                doc: None,
                                default: None,
                            },
                            StructField {
                                name: "height".into(),
                                ty: TypeRef::F32,
                                doc: None,
                                default: None,
                            },
                        ],
                    },
                    EnumVariant {
                        name: "Labeled".into(),
                        value: 3,
                        doc: None,
                        fields: vec![
                            StructField {
                                name: "label".into(),
                                ty: TypeRef::StringUtf8,
                                doc: None,
                                default: None,
                            },
                            StructField {
                                name: "count".into(),
                                ty: TypeRef::U8,
                                doc: None,
                                default: None,
                            },
                        ],
                    },
                ],
            },
            EnumDef {
                name: "Channel".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                ],
            },
        ],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }])
}

#[test]
fn rich_enum_renders_tagged_class_hierarchy() {
    let code = render(&shapes_api(), "Shapes", "weaveffi");

    // A rich enum is a tagged class hierarchy, never a plain constants
    // module and never an opaque pointer wrapper.
    assert!(
        !code.contains("module Shape\n"),
        "rich enum must not be a plain enum module: {code}"
    );
    assert!(code.contains("class Shape\n"), "rich enum base: {code}");
    assert!(
        !code.contains("ShapePtr") && !code.contains("FFI::AutoPointer"),
        "no pointer wrapper: {code}"
    );

    // One subclass per variant, carrying its TAG and fields.
    assert!(
        code.contains("class Empty < Shape") && code.contains("TAG = 0"),
        "unit variant: {code}"
    );
    assert!(
        code.contains("class Circle < Shape") && code.contains("TAG = 1"),
        "circle variant: {code}"
    );
    assert!(
        code.contains("class Labeled < Shape") && code.contains("TAG = 3"),
        "labeled variant: {code}"
    );
    assert!(code.contains("attr_reader :radius"), "circle field: {code}");
    assert!(
        code.contains("def initialize(radius:)"),
        "circle kwargs initialize: {code}"
    );
    assert!(
        code.contains("def initialize(width:, height:)"),
        "rectangle kwargs initialize: {code}"
    );
    assert!(
        code.contains("def tag") && code.contains("self.class::TAG"),
        "tag reader: {code}"
    );
    assert!(code.contains("def ==(other)"), "structural eq: {code}");

    // Rich enums own no C symbols at all.
    assert!(
        !code.contains("attach_function :weaveffi_shapes_Shape"),
        "no rich enum C symbols: {code}"
    );

    // Plain sibling enum still renders as a constants module.
    assert!(
        code.contains("module Channel"),
        "plain enum still a module: {code}"
    );
}

#[test]
fn rich_enum_codec_and_wrappers_use_value_buffers() {
    let code = render(&shapes_api(), "Shapes", "weaveffi");

    // The pack helper dispatches on the variant class and writes the tag
    // followed by the variant's fields; unknown objects trap.
    assert!(
        code.contains("def self._wv_write_shape(w, v)"),
        "pack helper: {code}"
    );
    assert!(
        code.contains("when Shape::Circle"),
        "variant dispatch: {code}"
    );
    let circle_pack = code
        .split("when Shape::Circle")
        .nth(1)
        .expect("circle pack arm");
    assert!(
        circle_pack.contains("w.write_i32(1)") && circle_pack.contains("w.write_f64(v.radius)"),
        "tag then fields: {code}"
    );
    assert!(
        code.contains("raise Error.new(-1, 'unknown Shape variant')"),
        "unknown variant trap: {code}"
    );

    // The unpack helper switches on the decoded tag and constructs the
    // matching subclass; unknown tags trap.
    assert!(
        code.contains("def self._wv_read_shape(r)"),
        "unpack helper: {code}"
    );
    assert!(code.contains("tag = r.read_i32"), "tag decode: {code}");
    assert!(
        code.contains("Shape::Circle.new(radius: _wv_radius)"),
        "circle construction: {code}"
    );
    assert!(
        code.contains("Shape::Rectangle.new(width: _wv_width, height: _wv_height)"),
        "rectangle construction: {code}"
    );
    assert!(
        code.contains("Shape::Empty.new"),
        "unit construction: {code}"
    );

    // A rich enum parameter packs into a value buffer and passes the
    // (ptr, len) slot pair; a rich enum return decodes from one.
    assert!(
        code.contains("def self.describe(shape)"),
        "describe wrapper: {code}"
    );
    assert!(
        code.contains("_wv_write_shape(shape_w, shape)"),
        "describe packs param: {code}"
    );
    assert!(
        code.contains("shape_buf, shape_data.bytesize"),
        "describe slot pair: {code}"
    );
    assert!(
        code.contains("_wv_value = _wv_read_shape(_wv_r)"),
        "scale decodes return: {code}"
    );
}

/// A mixed module exercising every buffered surface at once: records
/// nested in rich enums, buffered parameters and returns at module and
/// interface scope, a typed error with payload fields, a buffered async
/// result, a buffered iterator element, and a buffered listener argument.
#[test]
fn kitchen_sink_module_renders_coherently() {
    let mut m = simple_module(
        "kv",
        vec![
            {
                let mut f = plain_fn(
                    "kv_lookup",
                    vec![str_param("key")],
                    Some(TypeRef::Record("Entry".into())),
                );
                f.throws = true;
                f
            },
            plain_fn(
                "kv_tags",
                vec![Param {
                    name: "filter".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                    doc: None,
                }],
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            ),
            plain_fn(
                "kv_meta",
                vec![Param {
                    name: "entries".into(),
                    ty: TypeRef::List(Box::new(TypeRef::Record("Entry".into()))),
                    mutable: false,
                    doc: None,
                }],
                Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
            ),
            {
                let mut f = plain_fn(
                    "kv_load",
                    vec![],
                    Some(TypeRef::List(Box::new(TypeRef::RichEnum("Event".into())))),
                );
                f.r#async = true;
                f.throws = true;
                f
            },
            {
                let mut f = plain_fn(
                    "kv_scan",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
                );
                f.throws = true;
                f
            },
        ],
    );
    m.structs = vec![StructDef {
        name: "Entry".into(),
        doc: None,
        fields: vec![
            StructField {
                name: "key".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            },
            StructField {
                name: "hits".into(),
                ty: TypeRef::Optional(Box::new(TypeRef::U32)),
                doc: None,
                default: None,
            },
        ],
    }];
    m.enums = vec![EnumDef {
        name: "Event".into(),
        doc: None,
        variants: vec![
            EnumVariant {
                name: "Added".into(),
                value: 0,
                doc: None,
                fields: vec![StructField {
                    name: "entry".into(),
                    ty: TypeRef::Record("Entry".into()),
                    doc: None,
                    default: None,
                }],
            },
            EnumVariant {
                name: "Cleared".into(),
                value: 1,
                doc: None,
                fields: vec![],
            },
        ],
    }];
    m.errors = Some(ErrorDomain {
        name: "KvError".into(),
        codes: vec![ErrorCode {
            name: "KeyNotFound".into(),
            code: 1001,
            message: "key not found".into(),
            doc: None,
            fields: vec![StructField {
                name: "key".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }],
    });
    m.interfaces = vec![InterfaceDef {
        name: "Store".into(),
        doc: None,
        constructors: vec![plain_fn("new", vec![str_param("path")], None)],
        methods: vec![plain_fn(
            "put",
            vec![Param {
                name: "entry".into(),
                ty: TypeRef::Record("Entry".into()),
                mutable: false,
                doc: None,
            }],
            None,
        )],
        statics: vec![],
    }];
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    m.callbacks = vec![CallbackDef {
        name: "OnEvent".into(),
        params: vec![Param {
            name: "event".into(),
            ty: TypeRef::RichEnum("Event".into()),
            mutable: false,
            doc: None,
        }],
        doc: None,
    }];
    m.listeners = vec![ListenerDef {
        name: "event_listener".into(),
        event_callback: "OnEvent".into(),
        doc: None,
    }];
    let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
    // Codec calls inside a class body qualify the module receiver; at
    // module scope they stay bare.
    assert!(
        code.contains("WeaveFFI._wv_write_entry(entry_w, entry)"),
        "qualified codec call in interface method: {code}"
    );
    assert!(
        code.contains("_wv_write_entry(entries_w, _wv_e0)"),
        "list element pack at module scope: {code}"
    );
    // The rich enum codec recurses into the record codec for its
    // record-typed variant field.
    assert!(
        code.contains("_wv_entry = _wv_read_entry(r)"),
        "nested record decode in rich enum codec: {code}"
    );
    // The async list-of-rich-enum result decodes elementwise inside the
    // completion callback.
    assert!(
        code.contains("_wv_e0 = _wv_read_event(_wv_r)"),
        "async rich enum element decode: {code}"
    );
    // The iterator's buffered elements route through the record codec.
    assert!(
        code.contains("_wv_item = _wv_read_entry(_wv_r)"),
        "iterator element decode: {code}"
    );
    // The listener decodes the borrowed rich enum before the dispatch.
    assert!(
        code.contains("event_v = _wv_read_event(event_r)"),
        "listener rich enum decode: {code}"
    );
}

#[test]
fn async_buffered_result_decoded_inside_callback() {
    let api = make_api(vec![Module {
        name: "io".into(),
        functions: vec![{
            let mut f = plain_fn(
                "load_tags",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            );
            f.r#async = true;
            f
        }],
        interfaces: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // The borrowed result buffer is decoded inside the callback and
    // never freed (the producer frees after the callback returns).
    assert!(
        code.contains(
            "_wv_r = WvBufferReader.new(result_ptr.null? ? ''.b : result_ptr.read_string(result_len))"
        ),
        "borrowed buffer copied and decoded: {code}"
    );
    assert!(code.contains("queue << _wv_v"), "decoded push: {code}");
    assert!(
        !code.contains("weaveffi_free_bytes(result_ptr"),
        "borrowed callback buffer must not be freed: {code}"
    );
    // A decode failure surfaces through the queue rather than raising
    // across the C callback boundary.
    assert!(
        code.contains("rescue Error => e") && code.contains("queue << e"),
        "decode errors queued: {code}"
    );
}

#[test]
fn listener_buffered_argument_decoded_before_dispatch() {
    use weaveffi_ir::ir::{CallbackDef, ListenerDef};
    let api = make_api(vec![Module {
        callbacks: vec![CallbackDef {
            name: "OnUpdate".into(),
            params: vec![Param {
                name: "entry".into(),
                ty: TypeRef::Record("Entry".into()),
                mutable: false,
                doc: None,
            }],
            doc: None,
        }],
        listeners: vec![ListenerDef {
            name: "update_listener".into(),
            event_callback: "OnUpdate".into(),
            doc: None,
        }],
        structs: vec![StructDef {
            name: "Entry".into(),
            doc: None,
            fields: vec![StructField {
                name: "key".into(),
                ty: TypeRef::StringUtf8,
                doc: None,
                default: None,
            }],
        }],
        ..simple_module("events", vec![])
    }]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // The callback type declares the borrowed (ptr, len) slot pair.
    assert!(
        code.contains(
            "callback :weaveffi_events_OnUpdate_fn, [:pointer, :size_t, :pointer], :void"
        ),
        "callback decl: {code}"
    );
    // The trampoline decodes the borrowed buffer before the dispatch and
    // hands the block the decoded value.
    assert!(
        code.contains(
            "entry_r = WvBufferReader.new(entry_ptr.null? ? ''.b : entry_ptr.read_string(entry_len))"
        ),
        "borrowed arg decoded: {code}"
    );
    assert!(
        code.contains("_wv_entry_v = _wv_read_entry(entry_r)")
            || code.contains("entry_v = _wv_read_entry(entry_r)"),
        "record decode: {code}"
    );
    assert!(code.contains("block.call(entry_v)"), "dispatch: {code}");
    // A malformed buffer drops the event instead of raising across the
    // C callback boundary.
    assert!(
        code.contains("warn \"weaveffi: dropped OnUpdate event: #{e.message}\""),
        "malformed event dropped: {code}"
    );
}

#[test]
fn keyword_params_and_fields_gain_trailing_underscore() {
    let api = make_api(vec![Module {
        structs: vec![StructDef {
            name: "Config".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "end".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "normal".into(),
                    ty: TypeRef::Bool,
                    doc: None,
                    default: None,
                },
            ],
        }],
        ..simple_module(
            "kw",
            vec![plain_fn(
                "load",
                vec![str_param("end"), str_param("in"), str_param("key")],
                Some(TypeRef::I32),
            )],
        )
    }]);
    let code = render(&api, "WeaveFFI", "weaveffi");
    // Reserved parameter names are escaped everywhere they appear: the
    // wrapper signature and the C call. Unreserved names pass through.
    assert!(
        code.contains("def self.load(end_, in_, key)"),
        "escaped params: {code}"
    );
    assert!(
        code.contains("weaveffi_kw_load(end_, in_, key, err)"),
        "escaped call args: {code}"
    );
    // Reserved field names are escaped in accessors, keyword arguments,
    // assignments, equality, and the value-buffer codecs.
    assert!(code.contains("attr_reader :end_"), "accessor: {code}");
    assert!(
        code.contains("def initialize(end_:, normal:)"),
        "kwargs: {code}"
    );
    assert!(code.contains("@end_ = end_"), "assignment: {code}");
    assert!(
        code.contains("return false unless end_ == other.end_"),
        "equality: {code}"
    );
    assert!(
        code.contains("w.write_string(v.end_)"),
        "codec pack: {code}"
    );
    assert!(
        code.contains("Config.new(end_: _wv_end_, normal: _wv_normal)"),
        "codec unpack: {code}"
    );
    // No bare reserved spelling survives in an identifier position.
    assert!(!code.contains("attr_reader :end\n"), "raw keyword: {code}");
    assert!(!code.contains("def self.load(end,"), "raw param: {code}");
}

#[test]
fn negative_runtime_codes_fall_through_to_generic_error() {
    let code = render(&kv_api(), "WeaveFFI", "weaveffi");
    // The code-to-class table maps exactly the declared (positive) codes.
    let table = code
        .split("KV_ERROR_CODES = {")
        .nth(1)
        .expect("codes table")
        .split("}.freeze")
        .next()
        .expect("table body");
    assert!(table.contains("1001 => KvError::KeyNotFound,"), "{table}");
    assert!(table.contains("1004 => KvError::IoError,"), "{table}");
    assert!(!table.contains('-'), "no negative codes mapped: {table}");
    // The factory sends any code outside the table, the negative runtime
    // range included, to the generic branded Error.
    assert!(
        code.contains("cls = KV_ERROR_CODES[code]"),
        "table lookup: {code}"
    );
    assert!(
        code.contains("return Error.new(code, message) if cls.nil?"),
        "generic fallback: {code}"
    );
    // The trap path never consults the domain table: a non-throwing
    // function routes its out-err slot through the generic checker, which
    // raises the plain branded Error for panics and marshalling failures.
    let ping = code
        .split("def self.kv_ping()")
        .nth(1)
        .expect("kv_ping wrapper");
    let body = ping.split("\n  end").next().expect("wrapper body");
    assert!(body.contains("check_error!(err)"), "trap path: {code}");
    assert!(!body.contains("check_kv_error!"), "trap path: {code}");
    assert!(
        code.contains("raise Error.new(code, msg)"),
        "generic checker raises branded Error: {code}"
    );
}

#[test]
fn gemspec_escapes_quotes_and_backslashes_in_metadata() {
    use weaveffi_ir::ir::Package;

    let mut api = make_api(vec![simple_module("kv", vec![])]);
    api.package = Some(Package {
        name: "my-kv".into(),
        version: "1.2.3".into(),
        description: Some("A 'quoted' description with \\ backslash".into()),
        license: Some("MIT OR 'Custom'".into()),
        authors: vec!["O'Brien".into()],
        homepage: Some("https://example.com/it's".into()),
        repository: None,
    });
    let dir = tempfile::tempdir().unwrap();
    let out_dir = Utf8Path::from_path(dir.path()).unwrap();
    RubyGenerator
        .generate(&api, out_dir, &RubyConfig::default())
        .unwrap();
    let spec = std::fs::read_to_string(dir.path().join("ruby/my-kv.gemspec")).unwrap();
    assert!(
        spec.contains(r"s.summary     = 'A \'quoted\' description with \\ backslash'"),
        "summary escaped: {spec}"
    );
    assert!(
        spec.contains(r"s.license     = 'MIT OR \'Custom\''"),
        "license escaped: {spec}"
    );
    assert!(
        spec.contains(r"s.authors     = ['O\'Brien']"),
        "authors escaped: {spec}"
    );
    assert!(
        spec.contains(r"s.homepage    = 'https://example.com/it\'s'"),
        "homepage escaped: {spec}"
    );
}
