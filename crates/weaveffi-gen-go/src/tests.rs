//! Output-content tests for the Go generator: fixtures mirroring the
//! sample IDLs plus assertions over the rendered source.

use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef,
    ListenerDef, Module, Param, StructDef, StructField, TypeRef,
};

use super::*;

// ── Fixture helpers ──

fn api_of(modules: Vec<Module>) -> ResolvedApi {
    ResolvedApi::assume_resolved(Api {
        version: "0.7.0".into(),
        modules,
        generators: None,
        package: None,
    })
}

fn module(name: &str) -> Module {
    Module {
        name: name.into(),
        functions: vec![],
        interfaces: vec![],
        structs: vec![],
        enums: vec![],
        callbacks: vec![],
        listeners: vec![],
        errors: None,
        modules: vec![],
    }
}

fn func_of(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
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

fn throwing(mut f: Function) -> Function {
    f.throws = true;
    f
}

fn param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.into(),
        ty,
        mutable: false,
        doc: None,
    }
}

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn code(name: &str, value: i32, message: &str) -> ErrorCode {
    ErrorCode {
        name: name.into(),
        code: value,
        message: message.into(),
        doc: None,
        fields: vec![],
    }
}

fn variant(name: &str, value: i32, fields: Vec<StructField>) -> EnumVariant {
    EnumVariant {
        name: name.into(),
        value,
        doc: None,
        fields,
    }
}

/// Render with the default surface: `weaveffi` prefix, stripping on.
fn rg(api: &ResolvedApi) -> String {
    rg_with(api, "weaveffi", true)
}

fn rg_with(api: &ResolvedApi, prefix: &str, strip: bool) -> String {
    let model = BindingModel::build(api, prefix);
    render_go(api, &model, prefix, strip, "weaveffi.yml")
}

fn calculator_api() -> ResolvedApi {
    let mut m = module("calculator");
    m.functions = vec![
        func_of(
            "add",
            vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
            Some(TypeRef::I32),
        ),
        func_of(
            "echo",
            vec![param("msg", TypeRef::StringUtf8)],
            Some(TypeRef::StringUtf8),
        ),
    ];
    api_of(vec![m])
}

/// Mirrors `samples/kvstore/kvstore.yml`: the `Store` interface (ctor,
/// sync/async/iterator methods, a static), the `KvError` domain, the
/// `Entry` record, the eviction listener, and the nested `kv.stats`
/// submodule taking a cross-module interface parameter.
fn kv_api() -> ResolvedApi {
    let mut stats = module("stats");
    stats.structs = vec![StructDef {
        name: "Stats".into(),
        doc: None,
        fields: vec![field("total_entries", TypeRef::I64)],
    }];
    stats.functions = vec![throwing(func_of(
        "get_stats",
        // Cross-module references reach generators pre-qualified by the
        // validator's resolve step; mirror that spelling here.
        vec![param("store", TypeRef::Interface("kv.Store".into()))],
        Some(TypeRef::Record("Stats".into())),
    ))];

    let mut kv = module("kv");
    kv.errors = Some(ErrorDomain {
        name: "KvError".into(),
        codes: vec![
            code("KeyNotFound", 1001, "key not found"),
            code("Expired", 1002, "entry expired"),
            code("StoreFull", 1003, "store has reached capacity"),
            code("IoError", 1004, "I/O failure"),
        ],
    });
    kv.structs = vec![StructDef {
        name: "Entry".into(),
        doc: None,
        fields: vec![
            field("id", TypeRef::I64),
            field("key", TypeRef::StringUtf8),
            field("value", TypeRef::Bytes),
            field("expires_at", TypeRef::Optional(Box::new(TypeRef::I64))),
            field("tags", TypeRef::List(Box::new(TypeRef::StringUtf8))),
        ],
    }];
    kv.enums = vec![EnumDef {
        name: "EntryKind".into(),
        doc: None,
        variants: vec![
            variant("Volatile", 0, vec![]),
            variant("Persistent", 1, vec![]),
        ],
    }];
    kv.callbacks = vec![CallbackDef {
        name: "OnEvict".into(),
        doc: None,
        params: vec![param("key", TypeRef::StringUtf8)],
    }];
    kv.listeners = vec![ListenerDef {
        name: "eviction_listener".into(),
        event_callback: "OnEvict".into(),
        doc: None,
    }];
    kv.interfaces = vec![InterfaceDef {
        name: "Store".into(),
        doc: Some("An embedded key-value store owning its entries".into()),
        constructors: vec![throwing(func_of(
            "open",
            vec![param("path", TypeRef::StringUtf8)],
            None,
        ))],
        methods: vec![
            throwing(func_of(
                "put",
                vec![
                    param("key", TypeRef::StringUtf8),
                    param("value", TypeRef::Bytes),
                    param("kind", TypeRef::Enum("EntryKind".into())),
                    param("ttl_seconds", TypeRef::Optional(Box::new(TypeRef::I64))),
                ],
                Some(TypeRef::Bool),
            )),
            throwing(func_of(
                "get",
                vec![param("key", TypeRef::StringUtf8)],
                Some(TypeRef::Optional(Box::new(TypeRef::Record("Entry".into())))),
            )),
            throwing(func_of(
                "list_keys",
                vec![param(
                    "prefix",
                    TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                )],
                Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            )),
            func_of("count", vec![], Some(TypeRef::I64)),
            func_of("clear", vec![], None),
            {
                let mut f = throwing(func_of("compact", vec![], Some(TypeRef::I64)));
                f.r#async = true;
                f.cancellable = true;
                f
            },
            {
                let mut f = throwing(func_of(
                    "legacy_put",
                    vec![
                        param("key", TypeRef::StringUtf8),
                        param("value", TypeRef::Bytes),
                    ],
                    Some(TypeRef::Bool),
                ));
                f.deprecated = Some("use put() with explicit kind".into());
                f
            },
        ],
        statics: vec![func_of("default_capacity", vec![], Some(TypeRef::I64))],
    }];
    kv.modules = vec![stats];
    api_of(vec![kv])
}

/// Mirrors `samples/contacts/contacts.yml`, standing in for the CLI test
/// (`cli_go.rs`) while the workspace binary is blocked on other generator
/// crates mid-overhaul.
fn contacts_api() -> ResolvedApi {
    let mut m = module("contacts");
    m.enums = vec![EnumDef {
        name: "ContactType".into(),
        doc: None,
        variants: vec![
            variant("Personal", 0, vec![]),
            variant("Work", 1, vec![]),
            variant("Other", 2, vec![]),
        ],
    }];
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![
            field("id", TypeRef::I64),
            field("first_name", TypeRef::StringUtf8),
            field("last_name", TypeRef::StringUtf8),
            field("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            field("contact_type", TypeRef::Enum("ContactType".into())),
        ],
    }];
    m.errors = Some(ErrorDomain {
        name: "ContactsError".into(),
        codes: vec![
            code("InvalidName", 1, "name must not be empty"),
            code("NotFound", 2, "contact not found"),
        ],
    });
    m.interfaces = vec![InterfaceDef {
        name: "ContactBook".into(),
        doc: Some("An in-memory address book owning its contacts".into()),
        constructors: vec![func_of("new", vec![], None)],
        methods: vec![
            throwing(func_of(
                "add",
                vec![
                    param("first_name", TypeRef::StringUtf8),
                    param("last_name", TypeRef::StringUtf8),
                    param("email", TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    param("contact_type", TypeRef::Enum("ContactType".into())),
                ],
                Some(TypeRef::Record("Contact".into())),
            )),
            throwing(func_of(
                "get",
                vec![param("id", TypeRef::I64)],
                Some(TypeRef::Record("Contact".into())),
            )),
            func_of(
                "list",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
            ),
            func_of(
                "remove",
                vec![param("id", TypeRef::I64)],
                Some(TypeRef::Bool),
            ),
            func_of("count", vec![], Some(TypeRef::I32)),
        ],
        statics: vec![],
    }];
    api_of(vec![m])
}

/// A module with one rich (algebraic) enum used across params and
/// returns.
fn shapes_api() -> ResolvedApi {
    let mut m = module("shapes");
    m.enums = vec![EnumDef {
        name: "Shape".into(),
        doc: None,
        variants: vec![
            variant("Empty", 0, vec![]),
            variant("Circle", 1, vec![field("radius", TypeRef::F64)]),
            variant(
                "Labeled",
                3,
                vec![
                    field("label", TypeRef::StringUtf8),
                    field("count", TypeRef::U8),
                ],
            ),
        ],
    }];
    m.functions = vec![
        func_of(
            "describe",
            vec![param("shape", TypeRef::RichEnum("Shape".into()))],
            Some(TypeRef::StringUtf8),
        ),
        func_of(
            "scale",
            vec![
                param("shape", TypeRef::RichEnum("Shape".into())),
                param("factor", TypeRef::F64),
            ],
            Some(TypeRef::RichEnum("Shape".into())),
        ),
    ];
    api_of(vec![m])
}

// ── Scaffolding and packaging ──

#[test]
fn package_rewrites_cgo_and_bundles_libs() {
    use weaveffi_core::package::{FileContent, PackageContext};
    use weaveffi_core::platform::{BinarySet, Platform};

    let api = calculator_api();
    let model = BindingModel::build(&api, "weaveffi");
    let mut bins = BinarySet::new("calculator");
    bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
    bins.insert(Platform::WindowsX64, "/s/windows-x64/calculator.dll");
    let ctx = PackageContext {
        binaries: &bins,
        input_basename: Some("calculator.yml"),
    };
    // Mirror the CLI: the config basename drives the `-l<name>` link name,
    // which must match the bundled library's base name.
    let cfg = GoConfig {
        input_basename: Some("calculator.yml".into()),
        ..GoConfig::default()
    };
    let files = LanguageBackend::package(
        &GoGenerator,
        &api,
        &model,
        &ctx,
        Utf8Path::new("/out"),
        &cfg,
    )
    .expect("go supports packaging");

    assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
    let go = files
        .iter()
        .find(|f| f.path.as_str().ends_with("go/weaveffi.go"))
        .expect("go source present");
    let FileContent::Text(src) = &go.content else {
        panic!("go source is text");
    };
    assert!(
        src.contains("#cgo darwin,arm64 LDFLAGS: -L${SRCDIR}/lib/darwin-arm64"),
        "cgo preamble not rewritten: {src}"
    );
    assert!(src.contains("#cgo windows,amd64 LDFLAGS: -L${SRCDIR}/lib/windows-x64"));
    assert!(src.contains("#cgo LDFLAGS: -lcalculator"));
}

#[test]
fn name_returns_go() {
    assert_eq!(Generator::name(&GoGenerator), "go");
}

#[test]
fn output_files_correct() {
    let api = calculator_api();
    let out = Utf8Path::new("out");
    let files = GoGenerator.output_files(&api, out, &GoConfig::default());
    assert_eq!(
        files,
        vec![
            format!("{out}/go/README.md"),
            format!("{out}/go/go.mod"),
            format!("{out}/go/weaveffi.go"),
        ]
    );
}

#[test]
fn package_and_cgo_preamble() {
    let go = rg(&calculator_api());
    assert!(go.contains("package weaveffi\n"), "missing package");
    assert!(
        go.contains("#cgo LDFLAGS: -lweaveffi"),
        "missing LDFLAGS: {go}"
    );
    assert!(
        go.contains("#include \"weaveffi.h\""),
        "missing weaveffi.h include: {go}"
    );
    assert!(go.contains("import \"C\""), "missing import C: {go}");
}

#[test]
fn imports_fmt_and_unsafe() {
    let go = rg(&calculator_api());
    assert!(go.contains("\"fmt\""), "missing fmt import: {go}");
    assert!(go.contains("\"unsafe\""), "missing unsafe import: {go}");
}

// ── Plain (non-throwing) functions ──

#[test]
fn simple_i32_function() {
    let go = rg(&calculator_api());
    assert!(
        go.contains("func Add(a int32, b int32) int32 {"),
        "missing plain function sig: {go}"
    );
    assert!(
        go.contains("C.weaveffi_calculator_add("),
        "missing C call: {go}"
    );
    assert!(go.contains("C.int32_t(a)"), "missing param cast: {go}");
    assert!(go.contains("return int32(result)"), "missing return: {go}");
    assert!(
        !go.contains("return int32(result), nil"),
        "plain function must not return an error: {go}"
    );
}

#[test]
fn string_function() {
    let go = rg(&calculator_api());
    assert!(
        go.contains("func Echo(msg string) string {"),
        "missing echo sig: {go}"
    );
    assert!(go.contains("C.CString(msg)"), "missing CString: {go}");
    assert!(
        go.contains("defer C.free(unsafe.Pointer("),
        "missing defer free: {go}"
    );
    assert!(go.contains("C.GoString(result)"), "missing GoString: {go}");
    assert!(
        go.contains("C.weaveffi_free_string(result)"),
        "missing free_string: {go}"
    );
}

#[test]
fn plain_function_traps_on_error() {
    let go = rg(&calculator_api());
    assert!(
        go.contains("var cErr C.weaveffi_error"),
        "missing error var: {go}"
    );
    assert!(go.contains("wvTrap(&cErr)"), "missing trap check: {go}");
    assert!(
        go.contains("func wvTrap(cErr *C.weaveffi_error) {"),
        "missing wvTrap helper: {go}"
    );
    assert!(
        go.contains("C.weaveffi_error_clear(cErr)"),
        "missing error clear in wvTakeError: {go}"
    );
    assert!(
        go.contains("panic(fmt.Sprintf(\"weaveffi: %s (code %d)\", msg, code))"),
        "wvTrap must panic: {go}"
    );
}

#[test]
fn void_function() {
    let mut m = module("system");
    m.functions = vec![func_of("reset", vec![], None)];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Reset() {"),
        "missing plain void sig: {go}"
    );
    assert!(
        go.contains("wvTrap(&cErr)"),
        "plain void must trap on error: {go}"
    );
    assert!(
        !go.contains("func Reset() error"),
        "plain void must not return error: {go}"
    );
}

#[test]
fn handle_type() {
    let mut m = module("store");
    m.functions = vec![func_of(
        "create",
        vec![param("name", TypeRef::StringUtf8)],
        Some(TypeRef::Handle),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Create(name string) int64 {"),
        "handle return should be plain int64: {go}"
    );
    assert!(
        go.contains("return int64(result)"),
        "missing handle return conversion: {go}"
    );
}

#[test]
fn bool_function_generates_helpers() {
    let mut m = module("logic");
    m.functions = vec![func_of(
        "negate",
        vec![param("val", TypeRef::Bool)],
        Some(TypeRef::Bool),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(go.contains("func boolToC("), "missing boolToC: {go}");
    assert!(go.contains("func cToBool("), "missing cToBool: {go}");
    assert!(
        go.contains("boolToC(val)"),
        "missing boolToC call for param: {go}"
    );
    assert!(
        go.contains("cToBool(result)"),
        "missing cToBool for return: {go}"
    );
}

#[test]
fn enum_param_and_return() {
    let mut m = module("paint");
    m.functions = vec![func_of(
        "mix",
        vec![param("a", TypeRef::Enum("Color".into()))],
        Some(TypeRef::Enum("Color".into())),
    )];
    m.enums = vec![EnumDef {
        name: "Color".into(),
        doc: None,
        variants: vec![variant("Red", 0, vec![])],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Mix(a Color) Color {"),
        "missing enum function sig: {go}"
    );
    assert!(
        go.contains("C.weaveffi_paint_Color(a)"),
        "missing enum param conversion: {go}"
    );
    assert!(
        go.contains("Color(result)"),
        "missing enum return conversion: {go}"
    );
}

// ── Buffered params and returns ──

#[test]
fn struct_return_decodes_buffer() {
    let mut m = module("contacts");
    m.functions = vec![func_of(
        "get_contact",
        vec![param("id", TypeRef::Handle)],
        Some(TypeRef::Record("Contact".into())),
    )];
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("name", TypeRef::StringUtf8)],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func GetContact(id int64) Contact {"),
        "record return should be a bare value struct: {go}"
    );
    assert!(
        go.contains("var cOutLen C.size_t"),
        "missing out_len slot: {go}"
    );
    assert!(
        go.contains("rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}"),
        "buffered return must copy then free through wvCopyBuffer: {go}"
    );
    assert!(
        go.contains("goResult = wvUnpackContact(rRes)"),
        "missing record decode: {go}"
    );
    assert!(
        go.contains("rRes.expectEnd()"),
        "decoder must reject trailing bytes: {go}"
    );
    assert!(
        !go.contains("&Contact{ptr:"),
        "records no longer wrap C pointers: {go}"
    );
}

#[test]
fn buffered_record_param_packs() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("name", TypeRef::StringUtf8)],
    }];
    m.functions = vec![func_of(
        "save_contact",
        vec![param("contact", TypeRef::Record("Contact".into()))],
        None,
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func SaveContact(contact Contact) {"),
        "record param should be a bare value struct: {go}"
    );
    assert!(
        go.contains("wContact := &wvWriter{}"),
        "missing writer staging: {go}"
    );
    assert!(
        go.contains("wvPackContact(wContact, contact)"),
        "missing record pack call: {go}"
    );
    assert!(
        go.contains("cContactPtr = (*C.uint8_t)(unsafe.Pointer(&wContact.buf[0]))"),
        "missing buffer pointer staging: {go}"
    );
    assert!(
        go.contains(
            "C.weaveffi_contacts_save_contact(cContactPtr, C.size_t(len(wContact.buf)), &cErr)"
        ),
        "buffered param must pass ptr + len: {go}"
    );
}

#[test]
fn optional_string_param() {
    let mut m = module("store");
    m.functions = vec![func_of(
        "find",
        vec![param(
            "query",
            TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
        )],
        None,
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("query *string"),
        "optional string param should be *string: {go}"
    );
    assert!(
        go.contains("if query == nil {"),
        "missing nil check for optional: {go}"
    );
    assert!(
        go.contains("wQuery.writeOptionFlag(false)"),
        "missing absent flag write: {go}"
    );
    assert!(
        go.contains("wQuery.writeString((*query))"),
        "missing dereferenced string write: {go}"
    );
    assert!(
        go.contains("C.size_t(len(wQuery.buf))"),
        "optional param must pass the encoded length: {go}"
    );
}

#[test]
fn optional_struct_return() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("name", TypeRef::StringUtf8)],
    }];
    m.functions = vec![func_of(
        "find",
        vec![param("id", TypeRef::I32)],
        Some(TypeRef::Optional(Box::new(TypeRef::Record(
            "Contact".into(),
        )))),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Find(id int32) *Contact {"),
        "optional struct return: {go}"
    );
    assert!(
        go.contains("if rRes.readOptionFlag() {"),
        "missing option flag check: {go}"
    );
    assert!(
        go.contains("oRes0 = wvUnpackContact(rRes)"),
        "missing inner decode: {go}"
    );
    assert!(
        go.contains("goResult = &oRes0"),
        "present value must be pointer-wrapped: {go}"
    );
}

#[test]
fn list_return_decodes_buffer() {
    let mut m = module("store");
    m.functions = vec![func_of(
        "list_ids",
        vec![],
        Some(TypeRef::List(Box::new(TypeRef::I32))),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func ListIds() []int32 {"),
        "missing plain list return sig: {go}"
    );
    assert!(
        go.contains("var cOutLen C.size_t"),
        "missing out_len var: {go}"
    );
    assert!(
        go.contains("nRes0 := rRes.readLen()"),
        "missing count read: {go}"
    );
    assert!(
        go.contains("goResult = make([]int32, nRes0)"),
        "missing slice allocation: {go}"
    );
    assert!(
        go.contains("goResult[iRes0] = rRes.readI32()"),
        "missing element decode: {go}"
    );
}

#[test]
fn struct_list_return_decodes_elements() {
    let mut m = module("contacts");
    m.functions = vec![func_of(
        "list_contacts",
        vec![],
        Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
    )];
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("name", TypeRef::StringUtf8)],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func ListContacts() []Contact {"),
        "record lists hold values, not pointers: {go}"
    );
    assert!(
        go.contains("goResult[iRes0] = wvUnpackContact(rRes)"),
        "missing per-element record decode: {go}"
    );
}

#[test]
fn optional_i32_param_and_return() {
    let mut m = module("store");
    m.functions = vec![func_of(
        "find",
        vec![param("id", TypeRef::Optional(Box::new(TypeRef::I32)))],
        Some(TypeRef::Optional(Box::new(TypeRef::I32))),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("id *int32"),
        "optional i32 param should be *int32: {go}"
    );
    assert!(
        go.contains("wId.writeI32((*id))"),
        "missing dereferenced scalar write: {go}"
    );
    assert!(
        go.contains("var goResult *int32"),
        "optional i32 return should be *int32: {go}"
    );
    assert!(
        go.contains("oRes0 = rRes.readI32()"),
        "missing scalar decode: {go}"
    );
}

#[test]
fn map_return_decodes_buffer() {
    let mut m = module("store");
    m.functions = vec![func_of(
        "counts",
        vec![],
        Some(TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32),
        )),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Counts() map[string]int32 {"),
        "missing map return sig: {go}"
    );
    assert!(
        go.contains("goResult = make(map[string]int32, nRes0)"),
        "missing map allocation: {go}"
    );
    assert!(
        go.contains("kRes0 = rRes.readString()"),
        "missing key decode: {go}"
    );
    assert!(
        go.contains("vRes0 = rRes.readI32()"),
        "missing value decode: {go}"
    );
    assert!(
        go.contains("goResult[kRes0] = vRes0"),
        "missing map insert: {go}"
    );
}

#[test]
fn map_param_packs() {
    let mut m = module("metrics");
    m.functions = vec![func_of(
        "record_counts",
        vec![param(
            "counts",
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
        )],
        None,
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func RecordCounts(counts map[string]int32) {"),
        "missing map param sig: {go}"
    );
    assert!(
        go.contains("wCounts.writeLen(len(counts))"),
        "missing count write: {go}"
    );
    assert!(
        go.contains("for kCounts0, vCounts0 := range counts {"),
        "missing pair loop: {go}"
    );
    assert!(
        go.contains("wCounts.writeString(kCounts0)"),
        "missing key write: {go}"
    );
    assert!(
        go.contains("wCounts.writeI32(vCounts0)"),
        "missing value write: {go}"
    );
}

#[test]
fn optional_scalar_return_decodes_buffer() {
    let mut m = module("store");
    m.functions = vec![func_of(
        "capacity",
        vec![],
        Some(TypeRef::Optional(Box::new(TypeRef::I64))),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Capacity() *int64 {"),
        "optional scalar return should be a pointer: {go}"
    );
    assert!(
        go.contains("wvCopyBuffer(result, cOutLen)"),
        "buffered return must copy then free: {go}"
    );
    assert!(
        go.contains("oRes0 = rRes.readI64()"),
        "missing scalar decode: {go}"
    );
    assert!(go.contains("\t\"unsafe\"\n"), "unsafe import needed: {go}");
}

// ── Throwing functions ──

fn store_api() -> ResolvedApi {
    let mut m = module("store");
    m.errors = Some(ErrorDomain {
        name: "StoreError".into(),
        codes: vec![code("SaveFailed", 1, "save failed")],
    });
    m.functions = vec![
        throwing(func_of(
            "save",
            vec![param("data", TypeRef::StringUtf8)],
            Some(TypeRef::I32),
        )),
        throwing(func_of("flush", vec![], None)),
        func_of("clear", vec![], None),
    ];
    api_of(vec![m])
}

#[test]
fn throws_split_sync() {
    let go = rg(&store_api());
    // throws == true keeps `(T, error)` and maps through the domain.
    assert!(
        go.contains("func Save(data string) (int32, error) {"),
        "missing throwing sig: {go}"
    );
    assert!(
        go.contains("if cErr.code != 0 {"),
        "missing error check: {go}"
    );
    assert!(
        go.contains("return 0, wvMapStore(wvTakeError(&cErr))"),
        "throwing wrapper must map the domain error: {go}"
    );
    assert!(
        go.contains("return int32(result), nil"),
        "throwing wrapper must return `, nil` on success: {go}"
    );
    // Throwing void: `error` result, nil on success.
    assert!(
        go.contains("func Flush() error {"),
        "missing throwing void sig: {go}"
    );
    assert!(
        go.contains("return wvMapStore(wvTakeError(&cErr))"),
        "throwing void must return the mapped error: {go}"
    );
    assert!(go.contains("return nil"), "missing nil return: {go}");
    // throws == false stays plain and traps.
    assert!(
        go.contains("func Clear() {"),
        "missing plain void sig: {go}"
    );
    assert!(go.contains("wvTrap(&cErr)"), "missing trap: {go}");
}

#[test]
fn typed_error_surface() {
    let go = rg(&store_api());
    assert!(
        go.contains("type StoreError struct {"),
        "missing typed error struct: {go}"
    );
    assert!(
        go.contains("func (e *StoreError) Error() string {"),
        "typed error must implement error: {go}"
    );
    assert!(
        go.contains("StoreErrorSaveFailed int32 = 1"),
        "missing exported code constant: {go}"
    );
    assert!(
        go.contains("func wvMapStore(code int32, message string, payload []byte) error {"),
        "missing domain mapping helper: {go}"
    );
    assert!(
        go.contains("message = \"save failed\""),
        "missing default message fill: {go}"
    );
    assert!(
        go.contains("return wvBrandError(code, message, payload)"),
        "unknown codes must fall back to the brand error: {go}"
    );
    assert!(
        go.contains(&format!("type {ERROR_BRAND} struct {{")),
        "missing generic brand error: {go}"
    );
}

#[test]
fn wv_take_error_returns_payload() {
    let go = rg(&store_api());
    assert!(
        go.contains("func wvTakeError(cErr *C.weaveffi_error) (int32, string, []byte) {"),
        "wvTakeError must return the payload triple: {go}"
    );
    assert!(
        go.contains(
            "payload = C.GoBytes(unsafe.Pointer(cErr.payload_ptr), C.int(cErr.payload_len))"
        ),
        "wvTakeError must copy the payload before clearing: {go}"
    );
    assert!(
        go.contains("code, msg, _ := wvTakeError(cErr)"),
        "wvTrap discards the payload: {go}"
    );
}

#[test]
fn error_payload_fields_decode() {
    let mut m = module("store");
    m.errors = Some(ErrorDomain {
        name: "StoreError".into(),
        codes: vec![
            code("SaveFailed", 1, "save failed"),
            ErrorCode {
                name: "Conflict".into(),
                code: 2,
                message: "write conflict".into(),
                doc: None,
                fields: vec![
                    field("key", TypeRef::StringUtf8),
                    field("attempts", TypeRef::I32),
                ],
            },
        ],
    });
    m.functions = vec![throwing(func_of(
        "save",
        vec![param("data", TypeRef::StringUtf8)],
        None,
    ))];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("Payload any"),
        "domain with payload codes must expose Payload: {go}"
    );
    assert!(
        go.contains("type StoreErrorConflictPayload struct {"),
        "missing per-code payload struct: {go}"
    );
    assert!(
        go.contains("Key string") && go.contains("Attempts int32"),
        "payload struct must carry the declared fields: {go}"
    );
    assert!(
        go.contains("p.Key = r.readString()") && go.contains("p.Attempts = r.readI32()"),
        "payload fields must decode in wire order: {go}"
    );
    assert!(
        go.contains("e.Payload = p"),
        "decoded payload must attach to the error: {go}"
    );
    assert!(
        go.contains("r.expectEnd()"),
        "payload decode must reject trailing bytes: {go}"
    );
    // A code without fields keeps the simple construction.
    assert!(
        go.contains("return &StoreError{Code: code, Message: message}"),
        "codes without fields skip payload plumbing: {go}"
    );
}

// ── Enums, records, rich enums ──

#[test]
fn enum_generation() {
    let mut m = module("paint");
    m.enums = vec![EnumDef {
        name: "Color".into(),
        doc: None,
        variants: vec![
            variant("Red", 0, vec![]),
            variant("Green", 1, vec![]),
            variant("Blue", 2, vec![]),
        ],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("type Color int32"),
        "missing enum typedef: {go}"
    );
    assert!(
        go.contains("ColorRed Color = 0"),
        "missing Red variant: {go}"
    );
    assert!(
        go.contains("ColorGreen Color = 1"),
        "missing Green variant: {go}"
    );
    assert!(
        go.contains("ColorBlue Color = 2"),
        "missing Blue variant: {go}"
    );
}

#[test]
fn record_is_plain_value_struct() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![
            field("name", TypeRef::StringUtf8),
            field("age", TypeRef::I32),
        ],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(go.contains("type Contact struct {"), "missing struct: {go}");
    assert!(
        go.contains("\tName string\n"),
        "missing typed Name field: {go}"
    );
    assert!(
        go.contains("\tAge int32\n"),
        "missing typed Age field: {go}"
    );
    assert!(
        go.contains("func wvPackContact(w *wvWriter, v Contact) {"),
        "missing pack function: {go}"
    );
    assert!(
        go.contains("w.writeString(v.Name)") && go.contains("w.writeI32(v.Age)"),
        "pack must serialize fields in order: {go}"
    );
    assert!(
        go.contains("func wvUnpackContact(r *wvReader) Contact {"),
        "missing unpack function: {go}"
    );
    assert!(
        go.contains("v.Name = r.readString()") && go.contains("v.Age = r.readI32()"),
        "unpack must decode fields in order: {go}"
    );
    // Records have no C symbols: no handle wrapping, no destroy, no
    // getters, no builders.
    assert!(
        !go.contains("ptr *C.weaveffi_contacts_Contact"),
        "records must not wrap a C pointer: {go}"
    );
    assert!(
        !go.contains("Contact_destroy") && !go.contains("func (s *Contact)"),
        "records have no destroy or getters: {go}"
    );
    assert!(
        !go.contains("ContactBuilder"),
        "records have no builders: {go}"
    );
}

#[test]
fn record_optional_string_field() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field(
            "email",
            TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
        )],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("\tEmail *string\n"),
        "optional string field should be *string: {go}"
    );
    assert!(
        go.contains("if v.Email == nil {"),
        "pack must branch on presence: {go}"
    );
    assert!(
        go.contains("w.writeString((*v.Email))"),
        "pack must dereference the present value: {go}"
    );
    assert!(
        go.contains("oEmail0 = r.readString()") && go.contains("v.Email = &oEmail0"),
        "unpack must pointer-wrap the present value: {go}"
    );
}

#[test]
fn record_bytes_field_roundtrips() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![
            field("name", TypeRef::StringUtf8),
            field("photo", TypeRef::Bytes),
        ],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("w.writeBytes(v.Photo)"),
        "bytes fields pack as length-prefixed buffers: {go}"
    );
    assert!(
        go.contains("v.Photo = r.readBytes()"),
        "bytes fields decode as copies: {go}"
    );
}

#[test]
fn record_enum_field() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("contact_type", TypeRef::Enum("ContactType".into()))],
    }];
    m.enums = vec![EnumDef {
        name: "ContactType".into(),
        doc: None,
        variants: vec![variant("Personal", 0, vec![])],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("\tContactType ContactType\n"),
        "missing enum-typed field: {go}"
    );
    assert!(
        go.contains("w.writeI32(int32(v.ContactType))"),
        "enum fields pack as i32: {go}"
    );
    assert!(
        go.contains("v.ContactType = ContactType(r.readI32())"),
        "enum fields decode through the enum type: {go}"
    );
}

#[test]
fn rich_enum_is_sealed_sum_type() {
    let go = rg(&shapes_api());
    assert!(
        go.contains("type Shape interface {"),
        "missing sealed interface: {go}"
    );
    assert!(go.contains("\tisShape()\n"), "missing sealing method: {go}");
    assert!(
        go.contains("type ShapeEmpty struct{}"),
        "unit variant should be an empty struct: {go}"
    );
    assert!(
        go.contains("type ShapeCircle struct {") && go.contains("\tRadius float64\n"),
        "data variant carries typed fields: {go}"
    );
    assert!(
        go.contains("type ShapeLabeled struct {")
            && go.contains("\tLabel string\n")
            && go.contains("\tCount uint8\n"),
        "multi-field variant carries all fields: {go}"
    );
    assert!(
        go.contains("func (ShapeEmpty) isShape() {}")
            && go.contains("func (ShapeCircle) isShape() {}")
            && go.contains("func (ShapeLabeled) isShape() {}"),
        "every variant implements the sealing method: {go}"
    );
    // Rich enums have no C symbols.
    assert!(
        !go.contains("Shape_destroy") && !go.contains("NewShapeCircle") && !go.contains("Tag()"),
        "rich enums have no constructors, tag readers, or destroy: {go}"
    );
}

#[test]
fn rich_enum_pack_unpack() {
    let go = rg(&shapes_api());
    assert!(
        go.contains("func wvPackShape(w *wvWriter, v Shape) {"),
        "missing pack function: {go}"
    );
    assert!(
        go.contains("switch x := v.(type) {"),
        "pack switches on the variant type: {go}"
    );
    assert!(
        go.contains("case ShapeCircle:")
            && go.contains("w.writeI32(1)")
            && go.contains("w.writeF64(x.Radius)"),
        "pack writes the tag then the variant fields: {go}"
    );
    assert!(
        go.contains("case ShapeLabeled:")
            && go.contains("w.writeI32(3)")
            && go.contains("w.writeString(x.Label)")
            && go.contains("w.writeU8(x.Count)"),
        "non-contiguous tags use the declared values: {go}"
    );
    assert!(
        go.contains("panic(\"weaveffi: Shape value is not one of its variants\")"),
        "pack rejects foreign implementations: {go}"
    );
    assert!(
        go.contains("func wvUnpackShape(r *wvReader) Shape {"),
        "missing unpack function: {go}"
    );
    assert!(
        go.contains("return ShapeEmpty{}"),
        "unit variants decode to the empty struct: {go}"
    );
    assert!(
        go.contains("x.Radius = r.readF64()"),
        "variant fields decode in order: {go}"
    );
    assert!(
        go.contains("panic(\"weaveffi: malformed value buffer: Shape tag out of range\")"),
        "unpack rejects unknown tags: {go}"
    );
    // The rich enum crosses the ABI as a buffer in both directions.
    assert!(
        go.contains("func Describe(shape Shape) string {"),
        "rich enum param is the bare interface type: {go}"
    );
    assert!(
        go.contains("wvPackShape(wShape, shape)"),
        "rich enum param packs through the writer: {go}"
    );
    assert!(
        go.contains("func Scale(shape Shape, factor float64) Shape {"),
        "rich enum return is the bare interface type: {go}"
    );
    assert!(
        go.contains("goResult = wvUnpackShape(rRes)"),
        "rich enum return decodes from the buffer: {go}"
    );
}

#[test]
fn no_bool_helpers_when_unneeded() {
    let mut m = module("math");
    m.functions = vec![func_of(
        "add",
        vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
        Some(TypeRef::I32),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        !go.contains("boolToC"),
        "should not include bool helpers: {go}"
    );
}

// ── The value-buffer runtime ──

#[test]
fn buffer_runtime_emitted_once() {
    let mut a = module("alpha");
    a.structs = vec![StructDef {
        name: "A".into(),
        doc: None,
        fields: vec![field("x", TypeRef::I32)],
    }];
    let mut b = module("beta");
    b.structs = vec![StructDef {
        name: "B".into(),
        doc: None,
        fields: vec![field("y", TypeRef::F32)],
    }];
    let go = rg(&api_of(vec![a, b]));
    assert_eq!(
        go.matches("type wvWriter struct {").count(),
        1,
        "runtime must be emitted exactly once: {go}"
    );
    assert_eq!(
        go.matches("type wvReader struct {").count(),
        1,
        "runtime must be emitted exactly once: {go}"
    );
    assert!(
        go.contains("binary.LittleEndian"),
        "wire format is little-endian: {go}"
    );
    assert!(
        go.contains("if !utf8.Valid(b) {"),
        "string decode must validate UTF-8: {go}"
    );
    assert!(
        go.contains("wvMalformed(\"length prefix exceeds remaining buffer\")"),
        "reader must reject oversized length prefixes: {go}"
    );
    assert!(
        go.contains("wvMalformed(\"trailing bytes after value\")"),
        "reader must reject trailing bytes: {go}"
    );
    assert!(
        go.contains("C.weaveffi_free_bytes(ptr, length)"),
        "wvCopyBuffer must free the producer buffer: {go}"
    );
    assert!(
        go.contains("\t\"encoding/binary\"\n")
            && go.contains("\t\"math\"\n")
            && go.contains("\t\"unicode/utf8\"\n"),
        "runtime imports must be present: {go}"
    );
}

#[test]
fn no_buffer_runtime_when_unneeded() {
    let go = rg(&calculator_api());
    assert!(
        !go.contains("wvWriter"),
        "scalar-only surfaces need no buffer runtime: {go}"
    );
    assert!(
        !go.contains("\"encoding/binary\""),
        "scalar-only surfaces must not import binary: {go}"
    );
}

// ── Typed handles ──

#[test]
fn typed_handle_wrapper_and_flow() {
    let mut m = module("vault");
    m.structs = vec![StructDef {
        name: "Session".into(),
        doc: None,
        fields: vec![field("token", TypeRef::TypedHandle("Token".into()))],
    }];
    m.functions = vec![
        func_of("open", vec![], Some(TypeRef::TypedHandle("Token".into()))),
        func_of(
            "revoke",
            vec![param("t", TypeRef::TypedHandle("Token".into()))],
            None,
        ),
    ];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("type TokenHandle struct {"),
        "missing handle wrapper: {go}"
    );
    assert!(
        go.contains("ptr *C.weaveffi_vault_Token"),
        "wrapper must hold the opaque C pointer: {go}"
    );
    assert!(
        go.contains("func Open() *TokenHandle {"),
        "handle return should be the wrapper pointer: {go}"
    );
    assert!(
        go.contains("return &TokenHandle{ptr: result}"),
        "missing handle wrap on return: {go}"
    );
    assert!(
        go.contains("C.weaveffi_vault_revoke(t.ptr, &cErr)"),
        "handle params pass the wrapped pointer: {go}"
    );
    // No destroy: a typed handle is a borrowed id.
    assert!(
        !go.contains("func (s *TokenHandle) Close()"),
        "typed handles owe no release call: {go}"
    );
    // Inside buffers the handle serializes as the pointer's u64 value.
    assert!(
        go.contains("w.writeU64(uint64(uintptr(unsafe.Pointer(v.Token.ptr))))"),
        "handle fields pack as u64: {go}"
    );
    assert!(
        go.contains(
            "v.Token = &TokenHandle{ptr: (*C.weaveffi_vault_Token)(unsafe.Pointer(uintptr(r.readU64())))}"
        ),
        "handle fields decode back into the wrapper: {go}"
    );
}

// ── Async ──

/// Async functions get a blocking wrapper: a registry-id context, an
/// exported completion trampoline, and a buffered channel the wrapper
/// waits on. The channel is buffered so the producer thread never blocks
/// on the send even if the waiter has already given up.
#[test]
fn go_async_generates_blocking_wrapper() {
    let mut m = module("io");
    m.functions = vec![
        {
            let mut f = func_of("read", vec![], Some(TypeRef::StringUtf8));
            f.r#async = true;
            f
        },
        func_of("write", vec![], None),
    ];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("//export goWv_weaveffi_io_read_callback"),
        "completion trampoline must be exported: {go}"
    );
    assert!(
        go.contains("extern void goWv_weaveffi_io_read_callback(void* context, weaveffi_error* err, char* result);"),
        "preamble must declare the trampoline const-free: {go}"
    );
    assert!(
        go.contains("C.weaveffi_io_read_async("),
        "async launcher must be invoked: {go}"
    );
    assert!(
        go.contains("func Read() string {"),
        "plain async wrapper must have a bare return: {go}"
    );
    assert!(
        go.contains("ch := make(chan wvOutcomeIoRead, 1)"),
        "wrapper must wait on a buffered outcome channel: {go}"
    );
    assert!(
        go.contains("panic(outcome.err)"),
        "plain async wrapper must panic on a reported error: {go}"
    );
    assert!(
        go.contains("ch <- wvOutcomeIoRead{err: wvBrandError(wvTakeBoxedError(err))}"),
        "plain async trampoline brands the error, never the domain: {go}"
    );
    assert!(
        go.contains("return outcome.val"),
        "plain async wrapper returns the outcome value: {go}"
    );
    // The completion callback owns its result buffers: copy, then free.
    assert!(
        go.contains("C.weaveffi_free_string(result)"),
        "owned async result buffers must be freed after copying: {go}"
    );
    assert!(
        go.contains("val = C.GoString(result)"),
        "async string results must be copied before the callback returns: {go}"
    );
    assert!(
        go.contains("// Blocks the calling goroutine until the async producer completes."),
        "async wrapper must document that it blocks: {go}"
    );
    assert!(
        go.contains("weaveffi_io_write"),
        "sync function should still be emitted: {go}"
    );
    assert!(go.contains("\t\"sync\"\n"), "sync import needed: {go}");
}

#[test]
fn async_cancellable_passes_null_token() {
    let mut m = module("tasks");
    m.functions = vec![{
        let mut f = func_of("run", vec![], Some(TypeRef::I32));
        f.r#async = true;
        f.cancellable = true;
        f
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Run() int32 {"),
        "async wrapper must be generated: {go}"
    );
    assert!(
        go.contains("C.weaveffi_tasks_run_async(nil, "),
        "cancel token must be passed as NULL: {go}"
    );
}

#[test]
fn async_record_result_decodes_owned_buffer() {
    let mut m = module("metrics");
    m.structs = vec![StructDef {
        name: "Stats".into(),
        doc: None,
        fields: vec![field("total", TypeRef::I64)],
    }];
    m.functions = vec![{
        let mut f = func_of("load", vec![], Some(TypeRef::Record("Stats".into())));
        f.r#async = true;
        f
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains(
            "extern void goWv_weaveffi_metrics_load_callback(void* context, weaveffi_error* err, uint8_t* result_ptr, size_t result_len);"
        ),
        "buffered async callback carries borrowed ptr + len slots: {go}"
    );
    assert!(
        go.contains("rRes := &wvReader{buf: wvCopyBuffer(result_ptr, result_len)}"),
        "async result buffer is owned: copied, then freed: {go}"
    );
    assert!(
        go.contains("val = wvUnpackStats(rRes)"),
        "async record result decodes inside the trampoline: {go}"
    );
    assert!(
        go.contains("func Load() Stats {"),
        "async record wrapper returns the value struct: {go}"
    );
}

// ── Listeners and callbacks ──

#[test]
fn listeners_generate_register_unregister() {
    let mut m = module("events");
    m.callbacks = vec![CallbackDef {
        name: "OnMessage".into(),
        doc: None,
        params: vec![param("message", TypeRef::StringUtf8)],
    }];
    m.listeners = vec![ListenerDef {
        name: "message_listener".into(),
        event_callback: "OnMessage".into(),
        doc: None,
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("//export goWv_weaveffi_events_OnMessage_fn"),
        "callback trampoline must be exported: {go}"
    );
    assert!(
        go.contains("extern void goWv_weaveffi_events_OnMessage_fn(char* message, void* context);"),
        "preamble must declare the trampoline: {go}"
    );
    assert!(
        go.contains("func RegisterMessageListener(callback func(message string)) uint64 {"),
        "register wrapper must be emitted with the stripped name: {go}"
    );
    assert!(
        go.contains("func UnregisterMessageListener(id uint64) {"),
        "unregister wrapper must be emitted with the stripped name: {go}"
    );
    assert!(
        go.contains("C.weaveffi_events_register_message_listener(C.weaveffi_events_OnMessage_fn(unsafe.Pointer(C.goWv_weaveffi_events_OnMessage_fn)), unsafe.Pointer(uintptr(ctxID)))"),
        "register must pass the shared trampoline and registry id: {go}"
    );
    assert!(
        go.contains("wvListenerCtx[id] = ctxID"),
        "subscription must retain the Go callback: {go}"
    );
}

#[test]
fn callback_buffered_param_decodes_borrowed_buffer() {
    let mut m = module("feed");
    m.callbacks = vec![CallbackDef {
        name: "OnBatch".into(),
        doc: None,
        params: vec![param("items", TypeRef::List(Box::new(TypeRef::StringUtf8)))],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains(
            "extern void goWv_weaveffi_feed_OnBatch_fn(uint8_t* items_ptr, size_t items_len, void* context);"
        ),
        "buffered callback param carries ptr + len slots: {go}"
    );
    assert!(
        go.contains("rArg0 := &wvReader{buf: wvBorrowBuffer(items_ptr, items_len)}"),
        "callback buffers are borrowed, never freed: {go}"
    );
    assert!(
        go.contains("arg0 = make([]string, nArg00)"),
        "list argument decodes before dispatch: {go}"
    );
    assert!(
        go.contains("cb(arg0)"),
        "decoded value is handed to the user callback: {go}"
    );
}

// ── Interfaces ──

#[test]
fn interface_wrapper_and_ctor() {
    let go = rg(&kv_api());
    assert!(
        go.contains("type Store struct {"),
        "missing interface wrapper struct: {go}"
    );
    assert!(
        go.contains("ptr *C.weaveffi_kv_Store"),
        "missing wrapped C pointer: {go}"
    );
    // Factory constructor: `open` -> `OpenStore`, throwing.
    assert!(
        go.contains("func OpenStore(path string) (*Store, error) {"),
        "missing factory constructor: {go}"
    );
    assert!(
        go.contains("result := C.weaveffi_kv_Store_open(cPath, &cErr)"),
        "ctor must call the member symbol: {go}"
    );
    assert!(
        go.contains("return nil, wvMapKv(wvTakeError(&cErr))"),
        "throwing ctor maps the domain error: {go}"
    );
    assert!(
        go.contains("return &Store{ptr: result}, nil"),
        "ctor wraps the owned pointer: {go}"
    );
}

#[test]
fn interface_new_ctor_naming() {
    let go = rg(&contacts_api());
    assert!(
        go.contains("func NewContactBook() *ContactBook {"),
        "ctor named `new` must surface as New<Type>: {go}"
    );
    assert!(
        go.contains("result := C.weaveffi_contacts_ContactBook_new(&cErr)"),
        "missing ctor symbol call: {go}"
    );
    assert!(
        go.contains("return &ContactBook{ptr: result}"),
        "plain ctor wraps without error: {go}"
    );
}

#[test]
fn interface_methods_pass_self() {
    let go = rg(&kv_api());
    // Throwing method: `(T, error)` with the receiver's ptr leading. The
    // optional scalar parameter is buffered now.
    assert!(
        go.contains(
            "func (s *Store) Put(key string, value []byte, kind EntryKind, ttlSeconds *int64) (bool, error) {"
        ),
        "missing throwing method: {go}"
    );
    assert!(
        go.contains("result := C.weaveffi_kv_Store_put(s.ptr, cKey, cValuePtr, cValueLen, C.weaveffi_kv_EntryKind(kind), cTtlSecondsPtr, C.size_t(len(wTtlSeconds.buf)), &cErr)"),
        "method must pass s.ptr and the buffered optional's ptr + len: {go}"
    );
    assert!(
        go.contains("wTtlSeconds.writeI64((*ttlSeconds))"),
        "optional scalar param packs into the writer: {go}"
    );
    assert!(
        go.contains("return false, wvMapKv(wvTakeError(&cErr))"),
        "throwing bool method returns its zero value with the error: {go}"
    );
    // Optional record return through a method decodes from the buffer.
    assert!(
        go.contains("func (s *Store) Get(key string) (*Entry, error) {"),
        "missing optional-return method: {go}"
    );
    assert!(
        go.contains("oRes0 = wvUnpackEntry(rRes)"),
        "optional record return decodes the present value: {go}"
    );
    // Plain method: bare return, traps.
    assert!(
        go.contains("func (s *Store) Count() int64 {"),
        "missing plain method: {go}"
    );
    assert!(
        go.contains("result := C.weaveffi_kv_Store_count(s.ptr, &cErr)"),
        "plain method must pass s.ptr: {go}"
    );
    // Plain void method.
    assert!(
        go.contains("func (s *Store) Clear() {"),
        "missing plain void method: {go}"
    );
    // Deprecated member keeps its notice.
    assert!(
        go.contains("// Deprecated: use put() with explicit kind"),
        "missing deprecation notice: {go}"
    );
}

#[test]
fn interface_static_naming() {
    let go = rg(&kv_api());
    assert!(
        go.contains("func StoreDefaultCapacity() int64 {"),
        "statics are package-level, namespaced by the type: {go}"
    );
    assert!(
        go.contains("C.weaveffi_kv_Store_default_capacity(&cErr)"),
        "static must call the member symbol without self: {go}"
    );
}

#[test]
fn interface_close_calls_destroy() {
    let go = rg(&kv_api());
    assert!(
        go.contains("func (s *Store) Close() {"),
        "missing Close: {go}"
    );
    assert!(
        go.contains("C.weaveffi_kv_Store_destroy(s.ptr)"),
        "Close must call the destroy symbol: {go}"
    );
}

#[test]
fn optional_interface_param_stays_pointer() {
    let mut m = module("kv");
    m.interfaces = vec![InterfaceDef {
        name: "Store".into(),
        doc: None,
        constructors: vec![],
        methods: vec![],
        statics: vec![],
    }];
    m.functions = vec![func_of(
        "inspect",
        vec![param(
            "store",
            TypeRef::Optional(Box::new(TypeRef::Interface("Store".into()))),
        )],
        None,
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Inspect(store *Store) {"),
        "optional interface param stays a nullable wrapper pointer: {go}"
    );
    assert!(
        go.contains("var cStore *C.weaveffi_kv_Store"),
        "missing nullable C pointer staging: {go}"
    );
    assert!(
        go.contains("cStore = store.ptr"),
        "present value passes the wrapped pointer: {go}"
    );
    assert!(
        !go.contains("wStore"),
        "optional interfaces are never buffered: {go}"
    );
}

#[test]
fn interface_async_method_throws() {
    let go = rg(&kv_api());
    assert!(
        go.contains("func (s *Store) Compact() (int64, error) {"),
        "async throwing method keeps (T, error): {go}"
    );
    assert!(
        go.contains("type wvOutcomeKvStoreCompact struct {"),
        "outcome type derives from the member symbol: {go}"
    );
    assert!(
        go.contains("//export goWv_weaveffi_kv_Store_compact_callback"),
        "member trampoline must be exported: {go}"
    );
    assert!(
        go.contains("C.weaveffi_kv_Store_compact_async(s.ptr, nil, "),
        "launch passes s.ptr then the NULL cancel token: {go}"
    );
    assert!(
        go.contains("ch <- wvOutcomeKvStoreCompact{err: wvMapKv(wvTakeBoxedError(err))}"),
        "trampoline maps the domain error: {go}"
    );
    assert!(
        go.contains("return 0, outcome.err"),
        "throwing async wrapper returns the outcome error: {go}"
    );
}

#[test]
fn interface_iterator_method_throws() {
    let go = rg(&kv_api());
    // A throwing iterator returns iter.Seq2[T, error]; the standard iter
    // package is imported.
    assert!(
        go.contains("func (s *Store) ListKeys(prefix *string) iter.Seq2[string, error] {"),
        "throwing iterator method returns iter.Seq2[T, error]: {go}"
    );
    assert!(go.contains("\t\"iter\"\n"), "iter import needed: {go}");
    // The launch runs lazily inside the returned closure (first pull),
    // never in the wrapper body itself. The optional string param is
    // buffered and staged inside the closure.
    let fn_start = go
        .find("func (s *Store) ListKeys(")
        .expect("ListKeys wrapper");
    let fn_text = &go[fn_start..];
    let closure = fn_text
        .find("return func(yield func(string, error) bool) {")
        .expect("sequence closure in ListKeys");
    let launch = fn_text
        .find(
            "it := C.weaveffi_kv_Store_list_keys(s.ptr, cPrefixPtr, C.size_t(len(wPrefix.buf)), &cErr)",
        )
        .expect("launch in ListKeys");
    assert!(
        closure < launch,
        "launch must run inside the closure: {fn_text}"
    );
    // Launch errors are yielded as the final (zero, err) pair.
    assert!(
        go.contains("yield(\"\", wvMapKv(wvTakeError(&cErr)))"),
        "launch errors are yielded through the domain: {go}"
    );
    // Destroy is deferred inside the closure so an early break still
    // destroys exactly once.
    assert!(
        go.contains("defer C.weaveffi_kv_Store_ListKeysIterator_destroy(it)"),
        "iterator destroy must be deferred inside the closure: {go}"
    );
    // One producer next call per consumer step.
    assert!(
        go.contains("ok := C.weaveffi_kv_Store_ListKeysIterator_next(it, &outItem, &iterErr) != 0"),
        "iterator must pull one element per step: {go}"
    );
    assert!(
        go.contains("yield(\"\", wvMapKv(wvTakeError(&iterErr)))"),
        "per-element errors are yielded through the domain: {go}"
    );
    // Each yielded string element is freed after copying.
    assert!(
        go.contains("item := C.GoString(outItem)\n\t\t\tC.weaveffi_free_string(outItem)"),
        "string elements must be freed after copying: {go}"
    );
    assert!(
        go.contains("if !yield(item, nil) {"),
        "elements are yielded with a nil error: {go}"
    );
    // No hidden drain into a slice.
    assert!(
        !fn_text[..fn_text.find("\n}\n").unwrap()].contains("append("),
        "iterator must not drain into a slice: {fn_text}"
    );
}

#[test]
fn plain_iterator_function_traps() {
    let mut m = module("events");
    m.functions = vec![func_of(
        "get_messages",
        vec![],
        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func GetMessages() iter.Seq[string] {"),
        "plain iterator returns iter.Seq[T]: {go}"
    );
    assert!(
        go.contains("return func(yield func(string) bool) {"),
        "plain iterator returns a single-value sequence closure: {go}"
    );
    assert!(
        go.contains("wvTrap(&cErr)"),
        "plain iterator traps launch errors: {go}"
    );
    assert!(
        go.contains("wvTrap(&iterErr)"),
        "plain iterator traps per-element errors: {go}"
    );
    assert!(
        go.contains("defer C.weaveffi_events_GetMessagesIterator_destroy(it)"),
        "plain iterator defers destroy inside the closure: {go}"
    );
    assert!(
        go.contains("if !yield(item) {"),
        "plain iterator yields bare elements: {go}"
    );
    assert!(
        !go.contains("func GetMessages() []string"),
        "plain iterator must not drain into a slice: {go}"
    );
}

#[test]
fn iterator_buffered_elements_decode_and_free() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("name", TypeRef::StringUtf8)],
    }];
    m.functions = vec![func_of(
        "iter_contacts",
        vec![],
        Some(TypeRef::Iterator(Box::new(TypeRef::Record(
            "Contact".into(),
        )))),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func IterContacts() iter.Seq[Contact] {"),
        "record iterator yields value structs: {go}"
    );
    assert!(
        go.contains("var outItem *C.uint8_t") && go.contains("var outLen C.size_t"),
        "buffered elements arrive as ptr + len slots: {go}"
    );
    assert!(
        go.contains("(it, &outItem, &outLen, &iterErr) != 0"),
        "next must pass the element length slot: {go}"
    );
    assert!(
        go.contains("rItem := &wvReader{buf: wvCopyBuffer(outItem, outLen)}"),
        "each element must be copied and freed through wvCopyBuffer: {go}"
    );
    assert!(
        go.contains("item = wvUnpackContact(rItem)"),
        "each element decodes through the record unpack: {go}"
    );
    assert!(
        go.contains("rItem.expectEnd()"),
        "element decode must reject trailing bytes: {go}"
    );
}

#[test]
fn cross_module_interface_param_borrows() {
    let go = rg(&kv_api());
    assert!(
        go.contains("func GetStats(store *Store) (Stats, error) {"),
        "nested-module function takes the wrapper, returns the record value: {go}"
    );
    assert!(
        go.contains("result := C.weaveffi_kv_stats_get_stats(store.ptr, &cOutLen, &cErr)"),
        "interface params borrow the wrapped pointer: {go}"
    );
    assert!(
        go.contains("return Stats{}, wvMapKv(wvTakeError(&cErr))"),
        "inheriting submodule maps through the ancestor domain, zeroing the record: {go}"
    );
    assert!(
        go.contains("goResult = wvUnpackStats(rRes)"),
        "cross-module record return decodes from the buffer: {go}"
    );
}

#[test]
fn typed_error_emitted_once_with_all_codes() {
    let go = rg(&kv_api());
    assert_eq!(
        go.matches("type KvError struct {").count(),
        1,
        "domain type must be emitted exactly once: {go}"
    );
    assert!(go.contains("KvErrorKeyNotFound int32 = 1001"), "{go}");
    assert!(go.contains("KvErrorExpired int32 = 1002"), "{go}");
    assert!(go.contains("KvErrorStoreFull int32 = 1003"), "{go}");
    assert!(go.contains("KvErrorIoError int32 = 1004"), "{go}");
    assert!(
        go.contains("func wvMapKv(code int32, message string, payload []byte) error {"),
        "missing wvMapKv helper: {go}"
    );
    assert!(
        go.contains("case KvErrorKeyNotFound:"),
        "mapping must switch on the code constants: {go}"
    );
}

#[test]
fn kv_listener_uses_stripped_names() {
    let go = rg(&kv_api());
    assert!(
        go.contains("func RegisterEvictionListener(callback func(key string)) uint64 {"),
        "{go}"
    );
    assert!(
        go.contains("func UnregisterEvictionListener(id uint64) {"),
        "{go}"
    );
}

// ── Naming ──

#[test]
fn module_prefix_stripping_default_and_knob() {
    let api = calculator_api();
    let stripped = rg(&api);
    assert!(
        stripped.contains("func Add(a int32, b int32) int32 {"),
        "stripping is the default: {stripped}"
    );
    assert!(
        !stripped.contains("func CalculatorAdd("),
        "stripped output must not keep the module prefix: {stripped}"
    );
    let prefixed = rg_with(&api, "weaveffi", false);
    assert!(
        prefixed.contains("func CalculatorAdd(a int32, b int32) int32 {"),
        "knob off restores the module prefix: {prefixed}"
    );
}

#[test]
fn nested_module_stripping() {
    let go = rg_with(&kv_api(), "weaveffi", false);
    assert!(
        go.contains("func KvStatsGetStats(store *Store)"),
        "unstripped nested-module functions carry the full path: {go}"
    );
    // Interface members are namespaced by their type, never the module.
    assert!(
        go.contains("func (s *Store) Put("),
        "interface members are unaffected by the knob: {go}"
    );
    assert!(
        go.contains("func OpenStore(path string)"),
        "constructors are unaffected by the knob: {go}"
    );
}

#[test]
fn contacts_surface_matches_cli_expectations() {
    let go = rg(&contacts_api());
    assert!(go.contains("type ContactType int32"), "{go}");
    assert!(go.contains("type Contact struct {"), "{go}");
    assert!(go.contains("\tFirstName string\n"), "{go}");
    assert!(go.contains("\tEmail *string\n"), "{go}");
    assert!(go.contains("type ContactBook struct {"), "{go}");
    assert!(go.contains("ptr *C.weaveffi_contacts_ContactBook"), "{go}");
    assert!(
        go.contains("func (s *ContactBook) Add(firstName string, lastName string, email *string, contactType ContactType) (Contact, error) {"),
        "{go}"
    );
    assert!(
        go.contains("func (s *ContactBook) Get(id int64) (Contact, error) {"),
        "{go}"
    );
    assert!(
        go.contains("func (s *ContactBook) List() []Contact {"),
        "{go}"
    );
    assert!(
        go.contains("func (s *ContactBook) Remove(id int64) bool {"),
        "{go}"
    );
    assert!(go.contains("func (s *ContactBook) Count() int32 {"), "{go}");
    assert!(go.contains("func (s *ContactBook) Close() {"), "{go}");
    assert!(
        go.contains("C.weaveffi_contacts_ContactBook_destroy(s.ptr)"),
        "{go}"
    );
    assert!(go.contains("type ContactsError struct {"), "{go}");
    assert!(go.contains("ContactsErrorInvalidName int32 = 1"), "{go}");
    assert!(go.contains("ContactsErrorNotFound int32 = 2"), "{go}");
    assert!(
        go.contains("func wvMapContacts(code int32, message string, payload []byte) error {"),
        "{go}"
    );
    assert!(
        go.contains("return Contact{}, wvMapContacts(wvTakeError(&cErr))"),
        "{go}"
    );
}

// ── Generate-to-disk paths ──

#[test]
fn generates_file_on_disk() {
    let api = calculator_api();
    let tmp = std::env::temp_dir().join("weaveffi_test_go_gen");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

    GoGenerator
        .generate(&api, out_dir, &GoConfig::default())
        .unwrap();

    let go_file = tmp.join("go/weaveffi.go");
    assert!(go_file.exists(), "go/weaveffi.go should exist");
    let contents = std::fs::read_to_string(&go_file).unwrap();
    assert!(
        contents.contains("package weaveffi"),
        "file should contain package declaration"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn go_generates_go_mod() {
    let api = calculator_api();
    let tmp = std::env::temp_dir().join("weaveffi_test_go_mod");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    GoGenerator
        .generate(&api, out_dir, &GoConfig::default())
        .unwrap();

    let go_mod_path = tmp.join("go/go.mod");
    assert!(go_mod_path.exists(), "go/go.mod should exist");
    let go_mod = std::fs::read_to_string(&go_mod_path).unwrap();
    assert!(
        go_mod.contains("module weaveffi"),
        "missing module directive: {go_mod}"
    );
    assert!(
        go_mod.contains("go 1.23"),
        "go.mod must require Go 1.23 for the iter package: {go_mod}"
    );

    let readme_path = tmp.join("go/README.md");
    assert!(readme_path.exists(), "go/README.md should exist");
    let readme = std::fs::read_to_string(&readme_path).unwrap();
    assert!(
        readme.contains("CGo"),
        "README should mention CGo: {readme}"
    );
    assert!(
        readme.contains("go build"),
        "README should mention go build: {readme}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn generate_go_basic() {
    let api = calculator_api();
    let tmp = std::env::temp_dir().join("weaveffi_test_go_basic");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    GoGenerator
        .generate(&api, out_dir, &GoConfig::default())
        .unwrap();

    let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
    assert!(go.contains("package weaveffi"), "missing package: {go}");
    assert!(
        go.contains("func Add(a int32, b int32) int32 {"),
        "missing add function: {go}"
    );
    assert!(
        go.contains("func Echo(msg string) string {"),
        "missing echo function: {go}"
    );

    let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
    assert!(
        go_mod.contains("module weaveffi"),
        "go.mod should have default module path: {go_mod}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn go_custom_module_path() {
    let api = calculator_api();
    let tmp = std::env::temp_dir().join("weaveffi_test_go_custom_mod");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

    let config = GoConfig {
        module_path: Some("github.com/myorg/mylib".into()),
        ..GoConfig::default()
    };
    GoGenerator.generate(&api, out_dir, &config).unwrap();

    let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
    assert!(
        go_mod.contains("module github.com/myorg/mylib"),
        "go.mod should use custom module path: {go_mod}"
    );
    assert!(
        !go_mod.contains("module weaveffi"),
        "go.mod should not use default path: {go_mod}"
    );

    let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
    assert!(
        go.contains("package weaveffi"),
        "Go source should still use weaveffi package: {go}"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

// ── Ordering and memory-safety details ──

#[test]
fn go_no_double_free_on_error() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("name", TypeRef::StringUtf8)],
    }];
    m.functions = vec![func_of(
        "find_contact",
        vec![param("name", TypeRef::StringUtf8)],
        Some(TypeRef::Record("Contact".into())),
    )];
    let go = rg(&api_of(vec![m]));

    let fn_start = go.find("func FindContact(").expect("FindContact wrapper");
    let fn_body = &go[fn_start..];
    let fn_end = fn_body.find("\n}\n").unwrap();
    let fn_text = &fn_body[..fn_end];

    assert!(
        !fn_text.contains("weaveffi_free_string(cName"),
        "borrowed string param must not be freed via weaveffi_free_string: {fn_text}"
    );

    let err_check = fn_text
        .find("wvTrap(&cErr)")
        .expect("trap check in FindContact");
    let decode = fn_text
        .find("wvCopyBuffer(result, cOutLen)")
        .expect("buffered decode in FindContact");
    assert!(
        err_check < decode,
        "error must be checked before decoding the return buffer: {fn_text}"
    );
}

#[test]
fn go_flag_check_on_optional_return() {
    let mut m = module("contacts");
    m.structs = vec![StructDef {
        name: "Contact".into(),
        doc: None,
        fields: vec![field("name", TypeRef::StringUtf8)],
    }];
    m.functions = vec![func_of(
        "find_contact",
        vec![param("id", TypeRef::I32)],
        Some(TypeRef::Optional(Box::new(TypeRef::Record(
            "Contact".into(),
        )))),
    )];
    let go = rg(&api_of(vec![m]));

    let fn_start = go.find("func FindContact(").expect("FindContact wrapper");
    let fn_body = &go[fn_start..];
    let fn_end = fn_body.find("\n}\n").unwrap();
    let fn_text = &fn_body[..fn_end];

    let flag_check = fn_text
        .find("if rRes.readOptionFlag() {")
        .expect("flag check in FindContact");
    let decode = fn_text
        .find("wvUnpackContact(rRes)")
        .expect("Contact decode in FindContact");
    assert!(
        flag_check < decode,
        "optional record return must check the flag before decoding: {fn_text}"
    );
}

#[test]
fn string_list_return_decodes_from_buffer() {
    let mut m = module("store");
    m.functions = vec![func_of(
        "list_keys",
        vec![],
        Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
    )];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}"),
        "list return decodes from one owned buffer: {go}"
    );
    assert!(
        go.contains("goResult[iRes0] = rRes.readString()"),
        "string elements decode in place: {go}"
    );
    assert!(
        !go.contains("unsafe.Slice("),
        "parallel-array decoding is gone: {go}"
    );
}

// ── Docs ──

fn doc_api() -> ResolvedApi {
    let mut m = module("docs");
    m.functions = vec![Function {
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
    }];
    m.structs = vec![StructDef {
        name: "Item".into(),
        doc: Some("An item we track.".into()),
        fields: vec![StructField {
            name: "id".into(),
            ty: TypeRef::I64,
            doc: Some("Stable id".into()),
        }],
    }];
    m.enums = vec![EnumDef {
        name: "Kind".into(),
        doc: Some("Kind of item.".into()),
        variants: vec![EnumVariant {
            name: "Small".into(),
            value: 0,
            doc: Some("A small one".into()),
            fields: vec![],
        }],
    }];
    api_of(vec![m])
}

#[test]
fn go_emits_doc_on_function() {
    let go = rg(&doc_api());
    assert!(go.contains("// DoThing: Performs a thing."), "{go}");
}

#[test]
fn go_emits_doc_on_struct() {
    let go = rg(&doc_api());
    assert!(go.contains("// Item: An item we track."), "{go}");
}

#[test]
fn go_emits_doc_on_enum_variant() {
    let go = rg(&doc_api());
    assert!(go.contains("// Kind: Kind of item."), "{go}");
    assert!(go.contains("// KindSmall: A small one"), "{go}");
}

#[test]
fn go_emits_doc_on_field() {
    let go = rg(&doc_api());
    assert!(go.contains("// Id: Stable id"), "{go}");
}

#[test]
fn go_emits_doc_on_param() {
    let go = rg(&doc_api());
    assert!(go.contains("// Parameters:"), "{go}");
    assert!(go.contains("//   - x: the input value"), "{go}");
}

#[test]
fn go_custom_prefix_threads_to_user_symbols() {
    let go = rg_with(&calculator_api(), "myffi", true);
    // User symbols adopt the configured prefix.
    assert!(
        go.contains("C.myffi_calculator_add("),
        "user symbol should use the custom prefix: {go}"
    );
    assert!(
        !go.contains("weaveffi_calculator_add"),
        "user symbol must not keep the default prefix: {go}"
    );
    // The cgo preamble includes the prefixed C header.
    assert!(
        go.contains("#include \"myffi.h\""),
        "cgo preamble should include the prefixed header: {go}"
    );
    // Runtime ABI helpers exported by weaveffi-abi stay literal.
    assert!(
        go.contains("C.weaveffi_free_string(result)"),
        "runtime helper weaveffi_free_string must stay literal: {go}"
    );
    assert!(
        go.contains("var cErr C.weaveffi_error"),
        "runtime helper weaveffi_error must stay literal: {go}"
    );
}

#[test]
fn reserved_go_keywords_escape_in_param_positions() {
    let mut m = module("meta");
    m.functions = vec![func_of(
        "frob",
        vec![
            param("type", TypeRef::I32),
            param("func", TypeRef::StringUtf8),
        ],
        Some(TypeRef::I32),
    )];
    m.callbacks = vec![CallbackDef {
        name: "OnRange".into(),
        doc: None,
        params: vec![param("range", TypeRef::StringUtf8)],
    }];
    let go = rg(&api_of(vec![m]));
    assert!(
        go.contains("func Frob(type_ int32, func_ string) int32 {"),
        "keyword params must gain a trailing underscore: {go}"
    );
    assert!(
        go.contains("C.int32_t(type_)"),
        "escaped param must thread into the scalar conversion: {go}"
    );
    assert!(
        go.contains("cFunc := C.CString(func_)"),
        "staging locals derive from the escaped spelling: {go}"
    );
    assert!(
        go.contains("cb := v.(func(range_ string))"),
        "callback signatures escape keyword params too: {go}"
    );
}

#[test]
fn negative_codes_fall_back_to_the_brand_error() {
    // The ABI reserves all negative codes for the runtime (-1 generic, -2
    // producer panic, -3 marshalling failure); domain codes are validated
    // positive-only. The mapping helper must therefore route any negative
    // code through its default arm to the generic brand error, never a typed
    // domain case.
    let go = rg(&store_api());
    let map_start = go.find("func wvMapStore(").expect("mapping helper");
    let map_text = &go[map_start..];
    let map_text = &map_text[..map_text.find("\n}\n").unwrap()];
    assert!(
        !map_text.contains("case -"),
        "no negative code may match a domain case: {map_text}"
    );
    assert!(
        map_text.contains("default:")
            && map_text.contains("return wvBrandError(code, message, payload)"),
        "unmatched (including negative) codes must brand generically: {map_text}"
    );
}
