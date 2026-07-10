use std::fmt::Write;

use weaveffi_core::abi::AbiParam;
use weaveffi_core::model::{
    AbiFn, BindingModel, CallShape, EnumBinding, FieldBinding, ModuleBinding, StructBinding,
};
use weaveffi_ir::ir::Api;

/// The body every generated stub carries until the producer fills it in.
const TODO_BODY: &str = "    todo!()\n";

/// Render `name: <rust-ffi-type>` for one lowered ABI slot. The interface
/// receiver slot is named `self` in the C header; that is a keyword in Rust,
/// so the stub renames it `self_`.
fn slot_decl(p: &AbiParam, prefix: &str) -> String {
    let name = if p.name == "self" { "self_" } else { &p.name };
    format!("{}: {}", name, p.ty.render_rust(prefix))
}

/// Join lowered ABI slots into a Rust parameter list.
fn slots_decl(params: &[AbiParam], prefix: &str) -> String {
    params
        .iter()
        .map(|p| slot_decl(p, prefix))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Emit a `#[no_mangle] pub extern "C"` stub from an explicit signature.
fn emit_stub(out: &mut String, symbol: &str, params: &str, ret: &str, body: &str) {
    out.push_str("#[no_mangle]\n");
    let _ = writeln!(out, "pub extern \"C\" fn {symbol}({params}){ret} {{");
    out.push_str(body);
    out.push_str("}\n\n");
}

/// Emit a stub directly from a lowered [`AbiFn`]. A `void` return omits the
/// `-> T`; every other return type is rendered via the shared Rust vocabulary.
fn emit_abi_fn(out: &mut String, f: &AbiFn, prefix: &str, body: &str) {
    let ret = if f.ret == weaveffi_core::abi::CType::Void {
        String::new()
    } else {
        format!(" -> {}", f.ret.render_rust(prefix))
    };
    emit_stub(out, &f.symbol, &slots_decl(&f.params, prefix), &ret, body);
}

/// Emit a getter stub: an implicit `ptr: *const {tag}` receiver, any trailing
/// `out_*` slots (e.g. `out_len`), and the field's C return type.
fn emit_getter(out: &mut String, tag: &str, field: &FieldBinding, prefix: &str) {
    let mut params = vec![format!("ptr: *const {tag}")];
    params.extend(field.getter_out_params.iter().map(|p| slot_decl(p, prefix)));
    let ret = format!(" -> {}", field.getter_ret.render_rust(prefix));
    emit_stub(
        out,
        &field.getter_symbol,
        &params.join(", "),
        &ret,
        TODO_BODY,
    );
}

/// Render Rust `#[no_mangle] extern "C"` producer stubs for every symbol the
/// generated C ABI exposes, with `todo!()` bodies for the author to fill in.
///
/// Every signature is lowered through the shared [`BindingModel`] and
/// [`render_rust`](weaveffi_core::abi::CType::render_rust), so a scaffolded
/// producer matches the generated header (and therefore every language binding)
/// by construction; there is no second, drift-prone ABI lowering here.
pub fn render_scaffold(api: &Api, c_prefix: &str) -> String {
    let model = BindingModel::build(api, c_prefix);
    let mut out = String::new();
    out.push_str("#![allow(unsafe_code)]\n");
    out.push_str("#![allow(clippy::not_unsafe_ptr_arg_deref)]\n\n");
    out.push_str("use std::os::raw::c_char;\n");
    out.push_str("use weaveffi_abi::{self as abi, *};\n\n");

    // `BindingModel::modules` is already flattened in pre-order, so a single
    // pass covers nested modules without re-deriving the joined path.
    for m in &model.modules {
        render_module(&mut out, m, c_prefix);
    }

    out.push_str("// Re-export the fixed WeaveFFI C ABI runtime surface\n");
    out.push_str("// (string/byte deallocation, error clearing, cancel-token lifecycle)\n");
    out.push_str("// so consumer wrappers can call into this cdylib.\n");
    out.push_str("abi::export_runtime!();\n");

    out
}

fn render_module(out: &mut String, m: &ModuleBinding, prefix: &str) {
    for e in &m.enums {
        if e.is_rich() {
            render_rich_enum_scaffold(out, e, prefix);
        }
    }
    for s in &m.structs {
        render_struct_scaffold(out, s, prefix);
    }
    // Module-scope callback function-pointer typedefs the producer invokes.
    for cb in &m.callbacks {
        let _ = writeln!(
            out,
            "pub type {} = extern \"C\" fn({});\n",
            cb.c_fn_type,
            slots_decl(&cb.abi_params, prefix)
        );
    }
    // Event listeners: a register/unregister pair bound to a callback.
    for l in &m.listeners {
        emit_stub(
            out,
            &l.register_symbol,
            &format!(
                "callback: {}, context: *mut std::ffi::c_void",
                l.callback_c_fn_type
            ),
            " -> u64",
            TODO_BODY,
        );
        emit_stub(out, &l.unregister_symbol, "id: u64", "", TODO_BODY);
    }
    for i in &m.interfaces {
        render_interface_scaffold(out, i, prefix);
    }
    for f in &m.functions {
        emit_callable(out, &f.shape, prefix);
    }
}

/// Emit the producer stubs for one lowered call shape: a plain symbol, an
/// async callback typedef plus launcher, or an iterator's opaque state type
/// with its `launch`/`next`/`destroy` triple.
fn emit_callable(out: &mut String, shape: &CallShape, prefix: &str) {
    match shape {
        CallShape::Sync(abi) => emit_abi_fn(out, abi, prefix, TODO_BODY),
        CallShape::Async(a) => {
            // The completion callback typedef, then the launcher (its
            // params already carry the callback + context slots).
            let _ = writeln!(
                out,
                "pub type {} = extern \"C\" fn({});\n",
                a.callback_type,
                slots_decl(&a.callback_params, prefix)
            );
            emit_abi_fn(
                out,
                &a.launch,
                prefix,
                "    todo!(\"spawn async work and call callback with result\")\n",
            );
        }
        CallShape::Iterator(it) => {
            let tag = &it.iter_tag;
            let _ = writeln!(out, "#[repr(C)]\npub struct {tag} {{");
            out.push_str("    // TODO: hold the iterator's streaming state\n");
            out.push_str("}\n\n");
            emit_abi_fn(out, &it.launch, prefix, TODO_BODY);
            emit_abi_fn(out, &it.next, prefix, TODO_BODY);
            emit_stub(
                out,
                &it.destroy_symbol,
                &format!("iter: *mut {tag}"),
                "",
                TODO_BODY,
            );
        }
    }
}

/// Emit the producer surface for an interface: the opaque object type, every
/// constructor (returning an owned `*mut {tag}`), every method (leading
/// `self_: *const {tag}` receiver slot), every static, and the
/// `{tag}_destroy` release hook. Symbols and signatures come straight from
/// the [`InterfaceBinding`], mirroring the generated C header exactly.
fn render_interface_scaffold(
    out: &mut String,
    i: &weaveffi_core::model::InterfaceBinding,
    prefix: &str,
) {
    let tag = &i.c_tag;
    let _ = writeln!(out, "#[repr(C)]\npub struct {tag} {{");
    out.push_str("    // TODO: hold the object's state\n");
    out.push_str("}\n\n");
    for c in &i.constructors {
        emit_callable(out, &c.shape, prefix);
    }
    for m in &i.methods {
        emit_callable(out, &m.shape, prefix);
    }
    for s in &i.statics {
        emit_callable(out, &s.shape, prefix);
    }
    emit_stub(
        out,
        &i.destroy_symbol,
        &format!("self_: *mut {tag}"),
        "",
        TODO_BODY,
    );
}

fn render_struct_scaffold(out: &mut String, s: &StructBinding, prefix: &str) {
    let tag = &s.c_tag;
    let _ = writeln!(out, "#[repr(C)]\npub struct {tag} {{");
    out.push_str("    // TODO: add fields\n");
    out.push_str("}\n\n");

    emit_abi_fn(out, &s.create, prefix, TODO_BODY);
    emit_stub(
        out,
        &s.destroy_symbol,
        &format!("ptr: *mut {tag}"),
        "",
        TODO_BODY,
    );
    for field in &s.fields {
        emit_getter(out, tag, field, prefix);
    }

    if let Some(b) = &s.builder {
        let bt = &b.builder_tag;
        let _ = writeln!(out, "#[repr(C)]\npub struct {bt} {{");
        out.push_str("    // TODO: accumulate fields until build()\n");
        out.push_str("}\n\n");
        emit_stub(out, &b.new_symbol, "", &format!(" -> *mut {bt}"), TODO_BODY);
        for (field, (_, setter)) in s.fields.iter().zip(&b.setters) {
            let params = std::iter::once(format!("builder: *mut {bt}"))
                .chain(field.value_params.iter().map(|p| slot_decl(p, prefix)))
                .collect::<Vec<_>>()
                .join(", ");
            emit_stub(out, setter, &params, "", TODO_BODY);
        }
        emit_stub(
            out,
            &b.build_symbol,
            &format!("builder: *mut {bt}, out_err: *mut weaveffi_error"),
            &format!(" -> *mut {tag}"),
            TODO_BODY,
        );
        emit_stub(
            out,
            &b.destroy_symbol,
            &format!("builder: *mut {bt}"),
            "",
            TODO_BODY,
        );
    }
}

/// Emit the producer surface for a rich (algebraic) enum: an opaque object type
/// plus, for every variant, a `{tag}_{Variant}_new` constructor and one
/// `{tag}_{Variant}_get_{field}` getter per associated field, followed by the
/// shared `{tag}_tag` reader and `{tag}_destroy`. The symbol names and lowered
/// signatures come straight from the [`BindingModel`], so they mirror the
/// generated C header exactly.
fn render_rich_enum_scaffold(out: &mut String, e: &EnumBinding, prefix: &str) {
    let Some(rich) = &e.rich else {
        return;
    };
    let tag = &e.c_tag;
    let _ = writeln!(out, "#[repr(C)]\npub struct {tag} {{");
    out.push_str("    // TODO: represent the active variant and its associated data\n");
    out.push_str("}\n\n");

    for v in &rich.variants {
        emit_abi_fn(out, &v.create, prefix, TODO_BODY);
        for field in &v.fields {
            emit_getter(out, tag, field, prefix);
        }
    }

    emit_stub(
        out,
        &rich.tag_symbol,
        &format!("ptr: *const {tag}"),
        " -> i32",
        TODO_BODY,
    );
    emit_stub(
        out,
        &rich.destroy_symbol,
        &format!("ptr: *mut {tag}"),
        "",
        TODO_BODY,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Api, Function, Module, Param, StructDef, StructField, TypeRef};

    fn minimal_api(functions: Vec<Function>, structs: Vec<StructDef>) -> Api {
        Api {
            version: "0.5.0".to_string(),
            modules: vec![Module {
                name: "calc".to_string(),
                functions,
                structs,
                enums: vec![],
                interfaces: vec![],
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
    fn scaffold_interface_emits_type_members_and_destroy() {
        use weaveffi_ir::ir::InterfaceDef;
        let mut api = minimal_api(vec![], vec![]);
        api.modules[0].interfaces.push(InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![Function {
                name: "open".into(),
                params: vec![Param {
                    name: "path".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: true,
                deprecated: None,
                since: None,
            }],
            methods: vec![Function {
                name: "count".into(),
                params: vec![],
                returns: Some(TypeRef::I64),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            statics: vec![],
        });
        let out = render_scaffold(&api, "weaveffi");
        assert!(out.contains("pub struct weaveffi_calc_Store {"));
        assert!(out.contains("fn weaveffi_calc_Store_open("));
        // The method's receiver slot is renamed self_ for Rust (self is a
        // keyword), while the C header keeps `self`.
        assert!(
            out.contains("fn weaveffi_calc_Store_count(self_: *const weaveffi_calc_Store"),
            "method stub should carry a renamed receiver slot: {out}"
        );
        assert!(out.contains("fn weaveffi_calc_Store_destroy(self_: *mut weaveffi_calc_Store)"));
    }

    #[test]
    fn scaffold_has_allow_unsafe() {
        let api = minimal_api(vec![], vec![]);
        let out = render_scaffold(&api, "weaveffi");
        assert!(out.contains("#![allow(unsafe_code)]"));
    }

    #[test]
    fn scaffold_imports_abi() {
        let api = minimal_api(vec![], vec![]);
        let out = render_scaffold(&api, "weaveffi");
        assert!(out.contains("use weaveffi_abi::{self as abi, *};"));
    }

    #[test]
    fn scaffold_includes_runtime_exports_via_macro() {
        let api = minimal_api(vec![], vec![]);
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("abi::export_runtime!();"),
            "scaffold must call the export_runtime! macro instead of hand-writing thunks: {out}"
        );
        assert!(
            !out.contains("fn weaveffi_free_string("),
            "scaffold should not hand-write runtime thunks; the macro emits them: {out}"
        );
        assert!(
            !out.contains("fn weaveffi_free_bytes("),
            "scaffold should not hand-write runtime thunks; the macro emits them: {out}"
        );
        assert!(
            !out.contains("fn weaveffi_error_clear("),
            "scaffold should not hand-write runtime thunks; the macro emits them: {out}"
        );
    }

    #[test]
    fn scaffold_custom_prefix_renames_user_symbols() {
        let api = minimal_api(
            vec![Function {
                name: "add".into(),
                params: vec![Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![StructDef {
                name: "Point".into(),
                doc: None,
                fields: vec![StructField {
                    name: "x".into(),
                    ty: TypeRef::F64,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
        );
        let out = render_scaffold(&api, "myffi");
        assert!(
            out.contains("pub extern \"C\" fn myffi_calc_add("),
            "user fn should adopt custom prefix: {out}"
        );
        assert!(
            out.contains("pub struct myffi_calc_Point"),
            "user struct should adopt custom prefix: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn myffi_calc_Point_create("),
            "struct create stub should adopt custom prefix: {out}"
        );
        assert!(
            !out.contains("weaveffi_calc_add"),
            "user fn should not retain default prefix: {out}"
        );
        assert!(
            !out.contains("weaveffi_calc_Point"),
            "user struct should not retain default prefix: {out}"
        );
        assert!(
            out.contains("abi::export_runtime!();"),
            "runtime exports must come from the export_runtime! macro: {out}"
        );
    }

    #[test]
    fn scaffold_function_i32() {
        let api = minimal_api(
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
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains(
                "pub extern \"C\" fn weaveffi_calc_add(a: i32, b: i32, out_err: *mut weaveffi_error) -> i32 {"
            ),
            "missing add stub: {out}"
        );
        assert!(out.contains("todo!()"));
    }

    #[test]
    fn scaffold_function_string_param_and_return() {
        let api = minimal_api(
            vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "s".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains(
                "pub extern \"C\" fn weaveffi_calc_echo(s: *const c_char, out_err: *mut weaveffi_error) -> *const c_char {"
            ),
            "missing echo stub: {out}"
        );
    }

    #[test]
    fn scaffold_function_bytes_return_has_out_len() {
        let api = minimal_api(
            vec![Function {
                name: "data".into(),
                params: vec![],
                returns: Some(TypeRef::Bytes),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("out_len: *mut usize"),
            "bytes return should add out_len param: {out}"
        );
        // Matches the C header's `const uint8_t*` return exactly (the producer
        // hands back a buffer the consumer frees via `weaveffi_free_bytes`).
        assert!(
            out.contains("-> *const u8"),
            "bytes return type should be *const u8: {out}"
        );
    }

    #[test]
    fn scaffold_void_function() {
        let api = minimal_api(
            vec![Function {
                name: "reset".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_calc_reset(out_err: *mut weaveffi_error) {"),
            "missing void function: {out}"
        );
    }

    #[test]
    fn scaffold_struct_stubs() {
        let api = minimal_api(
            vec![],
            vec![StructDef {
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
                        name: "y".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(out.contains("#[repr(C)]"), "struct should be repr(C)");
        assert!(
            out.contains("pub struct weaveffi_calc_Point"),
            "missing struct definition: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_create(x: f64, y: f64, out_err: *mut weaveffi_error) -> *mut weaveffi_calc_Point"),
            "missing create stub: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_destroy(ptr: *mut weaveffi_calc_Point)"),
            "missing destroy stub: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_get_x(ptr: *const weaveffi_calc_Point) -> f64"),
            "missing x getter: {out}"
        );
        assert!(
            out.contains("fn weaveffi_calc_Point_get_y(ptr: *const weaveffi_calc_Point) -> f64"),
            "missing y getter: {out}"
        );
    }

    #[test]
    fn scaffold_rich_enum_emits_variant_surface() {
        use weaveffi_ir::ir::{EnumDef, EnumVariant};
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "shapes".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Shape".into(),
                    doc: None,
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
                    ],
                }],
                interfaces: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("pub struct weaveffi_shapes_Shape"),
            "opaque enum object type missing: {out}"
        );
        assert!(
            out.contains("fn weaveffi_shapes_Shape_Empty_new(out_err: *mut weaveffi_error) -> *mut weaveffi_shapes_Shape"),
            "unit-variant constructor missing: {out}"
        );
        assert!(
            out.contains("fn weaveffi_shapes_Shape_Circle_new(radius: f64, out_err: *mut weaveffi_error) -> *mut weaveffi_shapes_Shape"),
            "data-variant constructor missing: {out}"
        );
        assert!(
            out.contains("fn weaveffi_shapes_Shape_Circle_get_radius(ptr: *const weaveffi_shapes_Shape) -> f64"),
            "variant field getter missing: {out}"
        );
        assert!(
            out.contains("fn weaveffi_shapes_Shape_tag(ptr: *const weaveffi_shapes_Shape) -> i32"),
            "tag reader missing: {out}"
        );
        assert!(
            out.contains("fn weaveffi_shapes_Shape_destroy(ptr: *mut weaveffi_shapes_Shape)"),
            "destructor missing: {out}"
        );
    }

    #[test]
    fn scaffold_c_style_enum_emits_no_producer_surface() {
        use weaveffi_ir::ir::{EnumDef, EnumVariant};
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "shapes".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![EnumDef {
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
                }],
                interfaces: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            !out.contains("weaveffi_shapes_Channel"),
            "plain C-style enum must not get producer stubs: {out}"
        );
    }

    #[test]
    fn scaffold_struct_string_field_getter() {
        let api = minimal_api(
            vec![],
            vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("fn weaveffi_calc_Contact_get_name(ptr: *const weaveffi_calc_Contact) -> *const c_char"),
            "string getter should return *const c_char: {out}"
        );
    }

    #[test]
    fn scaffold_optional_value_param() {
        let api = minimal_api(
            vec![Function {
                name: "find".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("id: *const i32"),
            "optional i32 param should be pointer: {out}"
        );
    }

    #[test]
    fn scaffold_list_param() {
        let api = minimal_api(
            vec![Function {
                name: "sum".into(),
                params: vec![Param {
                    name: "items".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("items: *const i32, items_len: usize"),
            "list param should be ptr+len: {out}"
        );
    }

    #[test]
    fn scaffold_enum_param_uses_i32() {
        let api = minimal_api(
            vec![Function {
                name: "paint".into(),
                params: vec![Param {
                    name: "color".into(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("color: i32"),
            "enum param should be i32: {out}"
        );
        assert!(out.contains("-> i32"), "enum return should be i32: {out}");
    }

    #[test]
    fn scaffold_all_functions_have_no_mangle() {
        let api = minimal_api(
            vec![Function {
                name: "add".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        let no_mangle_count = out.matches("#[no_mangle]").count();
        let extern_count = out.matches("pub extern \"C\"").count();
        assert_eq!(
            no_mangle_count, extern_count,
            "every extern fn should have #[no_mangle]"
        );
    }

    #[test]
    fn scaffold_handle_type() {
        let api = minimal_api(
            vec![Function {
                name: "open".into(),
                params: vec![],
                returns: Some(TypeRef::Handle),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(out.contains("-> u64"), "handle return should be u64: {out}");
    }

    #[test]
    fn scaffold_with_map_type() {
        let api = minimal_api(
            vec![Function {
                name: "get_scores".into(),
                params: vec![Param {
                    name: "grades".into(),
                    ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("grades_keys: *const *const c_char"),
            "map param should have keys array: {out}"
        );
        assert!(
            out.contains("grades_values: *const i32"),
            "map param should have values array: {out}"
        );
        assert!(
            out.contains("grades_len: usize"),
            "map param should have length: {out}"
        );
        // A returned map is two callee-allocated parallel arrays handed back
        // through pointer-to-pointer out-params. These now lower through the
        // canonical `abi::lower_return`, so they match the header
        // (`const char*** out_keys, int32_t** out_values, size_t* out_len`)
        // exactly; the previous hand-rolled lowering dropped a level of
        // indirection on `out_values` and renamed the length to `out_map_len`.
        assert!(
            out.contains("out_keys: *mut *mut *const c_char"),
            "map return out_keys must match `const char***`: {out}"
        );
        assert!(
            out.contains("out_values: *mut *mut i32"),
            "map return out_values must match `int32_t**`: {out}"
        );
        assert!(
            out.contains("out_len: *mut usize"),
            "map return should use the canonical out_len: {out}"
        );
    }

    #[test]
    fn scaffold_typed_handle() {
        let api = minimal_api(
            vec![Function {
                name: "close".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::TypedHandle("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        // A typed handle crosses the ABI as the module's opaque tag pointer,
        // exactly as the header forward-declares it.
        assert!(
            out.contains("contact: *mut weaveffi_calc_Contact"),
            "TypedHandle param should be the opaque tag pointer: {out}"
        );
        assert!(
            out.contains("-> *mut weaveffi_calc_Contact"),
            "TypedHandle return should be the opaque tag pointer: {out}"
        );
    }

    #[test]
    fn scaffold_async_function() {
        let api = minimal_api(
            vec![Function {
                name: "fetch".into(),
                params: vec![Param {
                    name: "url".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("pub type weaveffi_calc_fetch_callback = extern \"C\" fn("),
            "missing callback type alias: {out}"
        );
        assert!(
            out.contains("callback: weaveffi_calc_fetch_callback"),
            "missing callback parameter: {out}"
        );
        assert!(
            out.contains("context: *mut std::ffi::c_void"),
            "missing context parameter: {out}"
        );
        assert!(
            out.contains("todo!(\"spawn async work and call callback with result\")"),
            "missing async todo body: {out}"
        );
        assert!(
            !out.contains("out_err: *mut weaveffi_error"),
            "async function should not have out_err param: {out}"
        );
        assert!(
            !out.contains("-> *const c_char"),
            "async function should not have a return type: {out}"
        );
    }

    #[test]
    fn scaffold_async_void_function() {
        let api = minimal_api(
            vec![Function {
                name: "sync_data".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        // The callback prefix is `(context, err)`, matching the header's
        // `typedef void (*..)(void* context, weaveffi_error* err)`. The earlier
        // hand-rolled scaffold emitted these in the reverse order.
        assert!(
            out.contains(
                "pub type weaveffi_calc_sync_data_callback = extern \"C\" fn(context: *mut std::ffi::c_void, err: *mut weaveffi_error);"
            ),
            "void async callback should be (context, err): {out}"
        );
        assert!(
            out.contains("callback: weaveffi_calc_sync_data_callback"),
            "missing callback parameter: {out}"
        );
    }

    #[test]
    fn scaffold_recurses_into_nested_modules_with_joined_path() {
        // A nested submodule's functions/structs must get stubs, and their C
        // symbols must use the underscore-joined module path so they line up
        // with the generated bindings.
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "graphics".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                interfaces: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![Module {
                    name: "shapes".into(),
                    functions: vec![Function {
                        name: "make".into(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        throws: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![StructDef {
                        name: "Shape".into(),
                        doc: None,
                        fields: vec![StructField {
                            name: "sides".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        }],
                        builder: false,
                    }],
                    enums: vec![],
                    interfaces: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                }],
            }],
            generators: None,
            package: None,
        };
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_graphics_shapes_make("),
            "nested fn should use joined path: {out}"
        );
        assert!(
            out.contains("pub struct weaveffi_graphics_shapes_Shape"),
            "nested struct should use joined path: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_graphics_shapes_Shape_create("),
            "nested struct create should use joined path: {out}"
        );
    }

    #[test]
    fn scaffold_qualified_cross_module_struct_ref_mangles_correctly() {
        // After resolution, a cross-module struct reference is dot-qualified
        // (e.g. `shared.Token`). The scaffold must flatten it to the owning
        // module's C symbol, never embed the dot or the referrer's module.
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "kitchen".into(),
                functions: vec![Function {
                    name: "cross".into(),
                    params: vec![],
                    returns: Some(TypeRef::Struct("shared.Token".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                interfaces: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("-> *mut weaveffi_shared_Token"),
            "qualified struct ref should mangle to owner module: {out}"
        );
        assert!(
            !out.contains("weaveffi_kitchen_shared"),
            "qualified ref must not embed the referrer module: {out}"
        );
        assert!(
            !out.contains("shared.Token"),
            "qualified ref must not leak the dotted name into Rust: {out}"
        );
    }

    #[test]
    fn scaffold_multiple_modules() {
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![
                Module {
                    name: "math".into(),
                    functions: vec![Function {
                        name: "add".into(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        throws: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![],
                    enums: vec![],
                    interfaces: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
                Module {
                    name: "io".into(),
                    functions: vec![Function {
                        name: "read".into(),
                        params: vec![],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        throws: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![],
                    enums: vec![],
                    interfaces: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
            ],
            generators: None,
            package: None,
        };
        let out = render_scaffold(&api, "weaveffi");
        assert!(out.contains("weaveffi_math_add"), "missing math module");
        assert!(out.contains("weaveffi_io_read"), "missing io module");
    }

    fn events_module(
        functions: Vec<Function>,
        callbacks: Vec<weaveffi_ir::ir::CallbackDef>,
        listeners: Vec<weaveffi_ir::ir::ListenerDef>,
    ) -> Api {
        Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "events".into(),
                functions,
                structs: vec![],
                enums: vec![],
                interfaces: vec![],
                callbacks,
                listeners,
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        }
    }

    #[test]
    fn scaffold_iterator_emits_opaque_type_next_and_destroy() {
        let api = events_module(
            vec![Function {
                name: "get_messages".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            vec![],
            vec![],
        );
        let out = render_scaffold(&api, "weaveffi");
        // Opaque iterator object the producer fills with streaming state.
        assert!(
            out.contains("pub struct weaveffi_events_GetMessagesIterator {"),
            "iterator opaque type missing: {out}"
        );
        // Launcher returns the opaque iterator pointer.
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_events_get_messages(out_err: *mut weaveffi_error) -> *mut weaveffi_events_GetMessagesIterator {"),
            "iterator launcher missing/incorrect: {out}"
        );
        // `next` writes the element through `out_item` and returns a status int.
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_events_GetMessagesIterator_next(iter: *mut weaveffi_events_GetMessagesIterator, out_item: *mut *const c_char, out_err: *mut weaveffi_error) -> i32 {"),
            "iterator next missing/incorrect: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_events_GetMessagesIterator_destroy(iter: *mut weaveffi_events_GetMessagesIterator) {"),
            "iterator destroy missing/incorrect: {out}"
        );
    }

    #[test]
    fn scaffold_listener_emits_callback_typedef_and_register_pair() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = events_module(
            vec![],
            vec![CallbackDef {
                name: "on_message".into(),
                params: vec![Param {
                    name: "text".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                doc: None,
            }],
            vec![ListenerDef {
                name: "messages".into(),
                event_callback: "on_message".into(),
                doc: None,
            }],
        );
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("pub type weaveffi_events_on_message_fn = extern \"C\" fn(text: *const c_char, context: *mut std::ffi::c_void);"),
            "module callback typedef missing/incorrect: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_events_register_messages(callback: weaveffi_events_on_message_fn, context: *mut std::ffi::c_void) -> u64 {"),
            "listener register missing/incorrect: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_events_unregister_messages(id: u64) {"),
            "listener unregister missing/incorrect: {out}"
        );
    }

    #[test]
    fn scaffold_struct_builder_emits_full_surface() {
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: true,
                }],
                enums: vec![],
                interfaces: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let out = render_scaffold(&api, "weaveffi");
        assert!(
            out.contains("pub struct weaveffi_contacts_ContactBuilder {"),
            "builder opaque type missing: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_contacts_Contact_Builder_new() -> *mut weaveffi_contacts_ContactBuilder {"),
            "builder new missing/incorrect: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_contacts_Contact_Builder_set_name(builder: *mut weaveffi_contacts_ContactBuilder, name: *const c_char) {"),
            "builder setter missing/incorrect: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_contacts_Contact_Builder_build(builder: *mut weaveffi_contacts_ContactBuilder, out_err: *mut weaveffi_error) -> *mut weaveffi_contacts_Contact {"),
            "builder build missing/incorrect: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_contacts_Contact_Builder_destroy(builder: *mut weaveffi_contacts_ContactBuilder) {"),
            "builder destroy missing/incorrect: {out}"
        );
    }

    #[test]
    fn scaffold_cancellable_async_threads_cancel_token() {
        let api = Api {
            version: "0.5.0".into(),
            modules: vec![Module {
                name: "net".into(),
                functions: vec![Function {
                    name: "fetch".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: true,
                    cancellable: true,
                    throws: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                interfaces: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let out = render_scaffold(&api, "weaveffi");
        // The completion callback prefix is (context, err, result).
        assert!(
            out.contains("pub type weaveffi_net_fetch_callback = extern \"C\" fn(context: *mut std::ffi::c_void, err: *mut weaveffi_error, result: *const c_char);"),
            "async callback typedef missing/incorrect: {out}"
        );
        // The cancel token slot sits before callback/context on the launcher.
        assert!(
            out.contains("cancel_token: *mut weaveffi_cancel_token"),
            "cancellable async must thread a cancel token: {out}"
        );
        assert!(
            out.contains("pub extern \"C\" fn weaveffi_net_fetch_async(id: i64, cancel_token: *mut weaveffi_cancel_token, callback: weaveffi_net_fetch_callback, context: *mut std::ffi::c_void) {"),
            "async launcher signature missing/incorrect: {out}"
        );
    }
}
