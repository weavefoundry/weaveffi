//! Ruby naming and the value descriptors the generated code hands the
//! runtime: the ffi gem's C type symbols, the *kinds* that say how a return,
//! an async result, or an iterator element crosses, the wire *types* that
//! say how a value is packed into a value buffer, and the YARD type of each
//! value.

use crate::lang::{escape_ident, escape_member, RUBY_KEYWORDS};
use heck::ToSnakeCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::errors::type_name;
use weaveffi_model::plan::{ErrorStrategy, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty, WireType};

/// Constants the runtime and the generated code refer to inside the gem's
/// module (its own classes, Ruby core classes, and the ffi gem). A user
/// type with one of these names would shadow it, so it gains a trailing `_`.
const RESERVED_CONSTANTS: &[&str] = &[
    "ABI_VERSION",
    "ArgumentError",
    "Array",
    "Bridge",
    "CancelToken",
    "Cancelled",
    "Class",
    "Comparable",
    "Data",
    "ENV",
    "Encoding",
    "EncodingError",
    "Enumerator",
    "Error",
    "Exception",
    "FFI",
    "File",
    "Float",
    "Hash",
    "Integer",
    "Kernel",
    "LoadError",
    "Module",
    "Monitor",
    "Mutex",
    "Native",
    "NativeBugError",
    "NoMemoryError",
    "NotImplementedError",
    "Numeric",
    "Object",
    "RangeError",
    "StandardError",
    "String",
    "Struct",
    "Symbol",
    "Thread",
    "TypeError",
];

/// The Ruby constant naming a user type (record, enum, interface, callback
/// interface, error domain, or variant): the IDL name, with a trailing `_`
/// when it would shadow a constant the bindings rely on (`Error`, `Data`,
/// `FFI`, ...).
pub(crate) fn rb_const(name: &str) -> String {
    if RESERVED_CONSTANTS.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The Ruby class of an error domain: the shared type name
/// ([`type_name`], so `KitchenErrors` is `KitchenError`), escaped like any
/// other constant.
pub(crate) fn rb_domain(name: &str) -> String {
    rb_const(&type_name(name, "Error"))
}

/// The Ruby spelling of a user-chosen parameter name: snake_case via heck,
/// then keyword-escaped (a reserved name like `end` gains a trailing `_`).
pub(crate) fn rb_param_name(name: &str) -> String {
    escape_ident(&name.to_snake_case(), RUBY_KEYWORDS)
}

/// The Ruby local holding one C slot of a call: the slot's name, keyword
/// escaped (a receiver's `self` slot is `self_`). Slot names are unique per
/// callable (validated), so these locals never collide.
pub(crate) fn rb_slot(slot: &AbiParam) -> String {
    escape_ident(&slot.name, RUBY_KEYWORDS)
}

/// Methods every record (a `Data` class) relies on, from `Data` and
/// `Object`. A field with one of these names gains a trailing `_`.
const RECORD_METHODS: &[&str] = &[
    "class",
    "clone",
    "deconstruct",
    "deconstruct_keys",
    "display",
    "dup",
    "extend",
    "freeze",
    "hash",
    "initialize",
    "inspect",
    "itself",
    "members",
    "method",
    "methods",
    "object_id",
    "public_send",
    "send",
    "singleton_class",
    "tap",
    "then",
    "to_h",
    "to_s",
    "with",
];

/// Methods every error class relies on, from `Exception` and the library's
/// `Error`. An error code's field with one of these names gains a
/// trailing `_`.
const ERROR_METHODS: &[&str] = &[
    "backtrace",
    "backtrace_locations",
    "cause",
    "class",
    "code",
    "detailed_message",
    "exception",
    "full_message",
    "hash",
    "inspect",
    "message",
    "object_id",
    "send",
    "set_backtrace",
    "to_s",
];

/// The Ruby name of a record or rich-enum variant field (a `Data` member):
/// verbatim, with a trailing `_` when it would replace a method the record
/// relies on. Keywords are fine as member names (`entry.end`).
pub(crate) fn rb_field_name(name: &str) -> String {
    escape_member(name, RECORD_METHODS)
}

/// The Ruby name of an error code's payload field: verbatim, with a
/// trailing `_` when it would replace a method the error relies on
/// (`code`, `message`, ...).
pub(crate) fn rb_error_field_name(name: &str) -> String {
    escape_member(name, ERROR_METHODS)
}

/// Instance methods every interface wrapper defines or relies on: the
/// runtime's `Handle` (`close`, `==`, `hash`, `inspect`, `initialize_copy`),
/// the constructor hook `initialize`, and the `Object` methods Ruby's own
/// object model calls. The runtime's private helpers start with `_wv_`,
/// which a snake-cased IDL name never does.
const OBJECT_METHODS: &[&str] = &[
    "class",
    "clone",
    "close",
    "display",
    "dup",
    "freeze",
    "hash",
    "initialize",
    "initialize_copy",
    "inspect",
    "method",
    "object_id",
    "send",
    "to_s",
];

/// Class and module methods the bindings rely on: `allocate` and `new`
/// (constructing wrappers), `name` (error messages and `inspect`), and the
/// `Module` reflection the runtime uses on the gem's module.
const MODULE_METHODS: &[&str] = &[
    "allocate",
    "ancestors",
    "class",
    "const_get",
    "constants",
    "freeze",
    "hash",
    "inspect",
    "name",
    "new",
    "object_id",
    "send",
    "to_s",
];

/// The Ruby spelling of an interface member: snake_case, with a trailing
/// `_` when it would replace a method the wrapper relies on, an instance
/// method for a `method`, else a class method (a static or factory). Ruby
/// allows keywords as method names, so no keyword escape applies.
pub(crate) fn rb_member_name(name: &str, method: bool) -> String {
    let members = if method {
        OBJECT_METHODS
    } else {
        MODULE_METHODS
    };
    escape_member(&name.to_snake_case(), members)
}

/// The Ruby spelling of a module-level function (a singleton method of the
/// gem's module).
pub(crate) fn rb_function_name(name: &str) -> String {
    escape_member(&name.to_snake_case(), MODULE_METHODS)
}

/// The Ruby spelling of a callback method, which an implementation defines
/// and the trampoline calls.
pub(crate) fn rb_callback_method_name(name: &str) -> String {
    escape_member(&name.to_snake_case(), OBJECT_METHODS)
}

/// The ffi gem's symbol for a C slot type. Every pointer (runs, objects,
/// out slots, vtables, contexts) is `:pointer`; `bool` is the 1-byte C
/// `bool`; a C-style enum is its `int32_t` typedef.
pub(crate) fn rb_ffi_type(ty: &CType) -> &'static str {
    match ty {
        CType::Bool => ":bool",
        CType::Int8 => ":int8",
        CType::Int16 => ":int16",
        CType::Int32 | CType::Enum { .. } => ":int32",
        CType::Uint8 => ":uint8",
        CType::Uint16 => ":uint16",
        CType::Uint32 => ":uint32",
        CType::Int64 => ":int64",
        CType::Uint64 => ":uint64",
        CType::Float => ":float",
        CType::Double => ":double",
        CType::Size => ":size_t",
        CType::Void => ":void",
        _ => ":pointer",
    }
}

/// The ffi gem's symbols for a list of slots, as a Ruby array literal.
pub(crate) fn rb_ffi_types(params: &[AbiParam]) -> String {
    let types: Vec<&str> = params.iter().map(|p| rb_ffi_type(&p.ty)).collect();
    format!("[{}]", types.join(", "))
}

/// The scalar kind of a direct value (`:i32`, `:f64`, `:bool`); a C-style
/// enum crosses as `:i32`.
pub(crate) fn rb_scalar(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => format!(":{}", p.snake()),
        Ty::Enum(_) => ":i32".to_string(),
        other => unreachable!("{other} isn't a scalar"),
    }
}

/// The scalar kind of a typed-array element.
pub(crate) fn rb_elem(p: Prim) -> String {
    format!(":{}", p.snake())
}

/// The scalar a range check applies to, when `ty` is an integer or a
/// C-style enum (floats and bools are converted by the ffi gem itself).
pub(crate) fn rb_checked_int(ty: &Ty) -> Option<String> {
    match ty {
        Ty::Prim(p) if p.is_integer() => Some(rb_scalar(ty)),
        Ty::Enum(_) => Some(rb_scalar(ty)),
        _ => None,
    }
}

/// The inner type of an optional.
fn optional_inner(ty: &Ty) -> &Ty {
    match ty {
        Ty::Optional(inner) => inner,
        other => unreachable!("{other} isn't optional"),
    }
}

/// The wire type of a value inside a value buffer: `:i64`, `:string`,
/// `[:opt, Item]`, `[:list, :string]`, `[:map, :string, :i32]`, or the class
/// of a record, rich enum, or interface. A C-style enum is `:i32`.
pub(crate) fn rb_wire(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!(":{}", p.snake()),
        WireType::Enum(_) => ":i32".to_string(),
        WireType::Object(n) | WireType::User(n) => rb_const(n),
        WireType::Optional(inner) => format!("[:opt, {}]", rb_wire(inner)),
        WireType::List(inner) => format!("[:list, {}]", rb_wire(inner)),
        WireType::Map(k, v) => format!("[:map, {}, {}]", rb_wire(k), rb_wire(v)),
    }
}

/// The kind of an object: `[:object, Store]` or `[:object?, Store]`.
fn object_kind(interface: &str, nullable: bool) -> String {
    let tag = if nullable { ":object?" } else { ":object" };
    format!("[{tag}, {}]", rb_const(interface))
}

/// The kind of a synchronous return (see the runtime's `Bridge` for the
/// vocabulary), or `None` for a void return. `ret` is the returned value
/// type. An iterator has no kind; its element does (see [`rb_item_kind`]).
pub(crate) fn rb_return_kind(pass: &RetPass, ret: Option<&Ty>) -> Option<String> {
    let ty = || ret.expect("a value return has a type");
    Some(match pass {
        RetPass::Void | RetPass::Iterator(_) => return None,
        RetPass::Direct => rb_scalar(ty()),
        RetPass::OptDirect { .. } => format!("[:opt, {}]", rb_scalar(optional_inner(ty()))),
        RetPass::Slice { elem, .. } => format!("[:slice, {}]", rb_elem(*elem)),
        RetPass::String { .. } => ":string".to_string(),
        RetPass::Bytes { .. } => ":bytes".to_string(),
        RetPass::Buffer { .. } => format!("[:buffer, {}]", rb_wire(ty())),
        RetPass::Object {
            nullable,
            interface,
            ..
        } => object_kind(interface, *nullable),
    })
}

/// The kind of an async result, `nil` for void.
pub(crate) fn rb_result_kind(pass: &ResultPass, ret: Option<&Ty>) -> String {
    let ty = || ret.expect("a value result has a type");
    match pass {
        ResultPass::Void => "nil".to_string(),
        ResultPass::Direct { .. } => rb_scalar(ty()),
        ResultPass::OptDirect { .. } => format!("[:opt, {}]", rb_scalar(optional_inner(ty()))),
        ResultPass::Slice { elem, .. } => format!("[:slice, {}]", rb_elem(*elem)),
        ResultPass::String { .. } => ":string".to_string(),
        ResultPass::Bytes { .. } => ":bytes".to_string(),
        ResultPass::Buffer { .. } => format!("[:buffer, {}]", rb_wire(ty())),
        ResultPass::Object {
            nullable,
            interface,
            ..
        } => object_kind(interface, *nullable),
    }
}

/// The kind of an iterator element of type `elem`.
pub(crate) fn rb_item_kind(pass: &ItemPass, elem: &Ty) -> String {
    match pass {
        ItemPass::Direct { .. } => rb_scalar(elem),
        ItemPass::OptDirect { .. } => format!("[:opt, {}]", rb_scalar(optional_inner(elem))),
        ItemPass::Slice { elem: p, .. } => format!("[:slice, {}]", rb_elem(*p)),
        ItemPass::String { .. } => ":string".to_string(),
        ItemPass::Bytes { .. } => ":bytes".to_string(),
        ItemPass::Buffer { .. } => format!("[:buffer, {}]", rb_wire(elem)),
        ItemPass::Object {
            nullable,
            interface,
            ..
        } => object_kind(interface, *nullable),
    }
}

/// The error class a call's failures are raised through: `nil` for a call
/// that declares no errors (a failure is a `NativeBugError`), `Error` for
/// `throws: any`, or the domain's class.
pub(crate) fn rb_error_arg(error: &ErrorStrategy) -> String {
    match error {
        ErrorStrategy::Trap => "nil".to_string(),
        ErrorStrategy::Untyped => "Error".to_string(),
        ErrorStrategy::Domain(name) => rb_domain(name),
    }
}

/// Escape a string for embedding in a single-quoted Ruby literal (the two
/// characters with meaning there: backslash and the quote itself).
pub(crate) fn rb_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// The YARD type of a value of `ty`, for `@param` and `@return` tags:
/// `Integer`, `String`, `Array<Entry>`, `Hash{String => Integer}`,
/// `Store, nil`.
pub(crate) fn rb_doc_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(Prim::Bool) => "Boolean".to_string(),
        Ty::Prim(Prim::F32 | Prim::F64) => "Float".to_string(),
        Ty::Prim(Prim::String | Prim::Bytes) => "String".to_string(),
        Ty::Prim(_) | Ty::Enum(_) => "Integer".to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Interface(n) => rb_const(n),
        Ty::Optional(inner) => format!("{}, nil", rb_doc_type(inner)),
        Ty::List(inner) => format!("Array<{}>", rb_doc_type(inner)),
        Ty::Map(k, v) => format!("Hash{{{} => {}}}", rb_doc_type(k), rb_doc_type(v)),
    }
}

/// The YARD type of a parameter: a value's, or a callback interface's
/// (`Listener`, `Listener, nil`).
pub(crate) fn rb_param_doc_type(ty: &ParamTy) -> String {
    match ty {
        ParamTy::Value(ty) => rb_doc_type(ty),
        ParamTy::Callback { name, nullable } => {
            let name = rb_const(name);
            if *nullable {
                format!("{name}, nil")
            } else {
                name
            }
        }
    }
}

/// The YARD type of a return: a value's, or an iterator's
/// (`Enumerator<Item>`).
pub(crate) fn rb_ret_doc_type(ty: &RetTy) -> String {
    match ty {
        RetTy::Value(ty) => rb_doc_type(ty),
        RetTy::Iterator(elem) => format!("Enumerator<{}>", rb_doc_type(elem)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_members_avoid_the_wrapper_methods() {
        assert_eq!(rb_member_name("close", true), "close_");
        assert_eq!(rb_member_name("initialize", true), "initialize_");
        assert_eq!(rb_member_name("name", true), "name");
        assert_eq!(rb_member_name("name", false), "name_");
        assert_eq!(rb_member_name("new", false), "new_");
        assert_eq!(rb_member_name("close", false), "close");
    }

    #[test]
    fn fields_avoid_the_methods_records_and_errors_rely_on() {
        assert_eq!(rb_field_name("end"), "end");
        assert_eq!(rb_field_name("hash"), "hash_");
        assert_eq!(rb_field_name("with"), "with_");
        assert_eq!(rb_field_name("message"), "message");
        assert_eq!(rb_error_field_name("message"), "message_");
        assert_eq!(rb_error_field_name("code"), "code_");
        assert_eq!(rb_error_field_name("key"), "key");
    }

    #[test]
    fn type_names_never_shadow_what_the_bindings_use() {
        assert_eq!(rb_const("Data"), "Data_");
        assert_eq!(rb_const("Error"), "Error_");
        assert_eq!(rb_const("Store"), "Store");
        assert_eq!(rb_domain("Error"), "Error_");
        assert_eq!(rb_domain("KitchenErrors"), "KitchenError");
        assert_eq!(rb_domain("Failure"), "FailureError");
    }

    #[test]
    fn wire_types_nest() {
        let ty = Ty::Map(
            Box::new(Ty::Prim(Prim::String)),
            Box::new(Ty::List(Box::new(Ty::Optional(Box::new(Ty::Record(
                "Data".into(),
            )))))),
        );
        assert_eq!(rb_wire(&ty), "[:map, :string, [:list, [:opt, Data_]]]");
        assert_eq!(rb_wire(&Ty::Enum("Color".into())), ":i32");
    }
}
