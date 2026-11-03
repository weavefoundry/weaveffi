//! Ruby naming and the value descriptors the generated code hands the
//! runtime: the ffi gem's C types, the *kinds* that say how a value crosses
//! (one per [`Crossing`], the same in every position), the wire *types* that
//! say how a value is packed into a value buffer, and the YARD type of each
//! value.

use crate::codegen::values::{crossing, Crossing};
use crate::lang::{escape_ident, escape_member, RUBY_KEYWORDS};
use heck::ToSnakeCase;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::errors::type_name;
use weaveffi_model::model::ErrorStrategy;
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
    "public_send",
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

/// The Ruby spelling of a library function (a singleton method of the gem's
/// module).
pub(crate) fn rb_function_name(name: &str) -> String {
    escape_member(&name.to_snake_case(), MODULE_METHODS)
}

/// The Ruby spelling of a callback method, which an implementation defines
/// and the trampoline calls.
pub(crate) fn rb_callback_method_name(name: &str) -> String {
    escape_member(&name.to_snake_case(), OBJECT_METHODS)
}

/// The ffi gem's type of a C slot: a scalar's symbol (`:int32`, `:bool`,
/// the 1-byte C `bool`), a C-style enum's `int32_t` typedef, a struct
/// passed by value (`Run.by_value`, `OptI32.by_value`, `Callback.by_value`,
/// resolved inside the private `Native` module), or `:pointer` for every
/// pointer (objects, iterators, the error slot, tokens, completions).
pub(crate) fn rb_ffi_type(ty: &CType) -> String {
    match ty {
        CType::Scalar(p) => format!(":{}", rb_ffi_scalar(*p)),
        CType::Enum(_) => ":int32".to_string(),
        CType::Size => ":size_t".to_string(),
        CType::Void => ":void".to_string(),
        CType::Opt(p) => format!("Opt{}.by_value", p.pascal()),
        CType::Run => "Run.by_value".to_string(),
        CType::Callback(_) => "Callback.by_value".to_string(),
        _ => ":pointer".to_string(),
    }
}

/// The ffi gem's name of a scalar primitive.
fn rb_ffi_scalar(p: Prim) -> &'static str {
    match p {
        Prim::Bool => "bool",
        Prim::I8 => "int8",
        Prim::I16 => "int16",
        Prim::I32 => "int32",
        Prim::I64 => "int64",
        Prim::U8 => "uint8",
        Prim::U16 => "uint16",
        Prim::U32 => "uint32",
        Prim::U64 => "uint64",
        Prim::F32 => "float",
        Prim::F64 => "double",
        Prim::String | Prim::Bytes => unreachable!("{p} isn't a scalar"),
    }
}

/// The ffi gem's types of a list of slots, as a Ruby array literal.
pub(crate) fn rb_ffi_types(params: &[AbiParam]) -> String {
    let types: Vec<String> = params.iter().map(|p| rb_ffi_type(&p.ty)).collect();
    format!("[{}]", types.join(", "))
}

/// The runtime's name of a scalar (`:i32`, `:f64`, `:bool`).
pub(crate) fn rb_scalar(p: Prim) -> String {
    format!(":{}", p.snake())
}

/// The wire type of a value inside a value buffer: `:i64`, `:string`,
/// `[:opt, Item]`, `[:list, :string]`, `[:map, :string, :i32]`, or the class
/// of a record, rich enum, or interface. A C-style enum is `:i32`.
pub(crate) fn rb_wire(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => rb_scalar(p),
        WireType::Enum(_) => ":i32".to_string(),
        WireType::Object(n) | WireType::User(n) => rb_const(n),
        WireType::Optional(inner) => format!("[:opt, {}]", rb_wire(inner)),
        WireType::List(inner) => format!("[:list, {}]", rb_wire(inner)),
        WireType::Map(k, v) => format!("[:map, {}, {}]", rb_wire(k), rb_wire(v)),
    }
}

/// The runtime's *kind* of a value of type `ty`, which says how it crosses
/// in every position (see the runtime's `Bridge`): a scalar (`:i32`; a
/// C-style enum is `:i32`), `[:opt, :i32]`, `:string`, `:bytes`,
/// `[:slice, :f64]`, `[:buffer, type]`, `[:object, Store]`, or
/// `[:object?, Store]`.
pub(crate) fn rb_kind(ty: &Ty) -> String {
    match crossing(ty) {
        Crossing::Scalar(p) => rb_scalar(p),
        Crossing::Enum(_) => ":i32".to_string(),
        Crossing::OptScalar(p) => format!("[:opt, {}]", rb_scalar(p)),
        Crossing::OptEnum(_) => "[:opt, :i32]".to_string(),
        Crossing::Utf8 => ":string".to_string(),
        Crossing::Bytes => ":bytes".to_string(),
        Crossing::Slice(p) => format!("[:slice, {}]", rb_scalar(p)),
        Crossing::Buffer(ty) => format!("[:buffer, {}]", rb_wire(ty)),
        Crossing::Object {
            interface,
            nullable,
        } => {
            let tag = if nullable { ":object?" } else { ":object" };
            format!("[{tag}, {}]", rb_const(interface))
        }
    }
}

/// The kind of a value that may be absent (a void return or result):
/// `nil` when it is.
pub(crate) fn rb_kind_or_nil(ty: Option<&Ty>) -> String {
    ty.map_or_else(|| "nil".to_string(), rb_kind)
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

    #[test]
    fn every_crossing_has_a_kind_and_one_ffi_type() {
        let opt = |t| Ty::Optional(Box::new(t));
        let list = |t| Ty::List(Box::new(t));
        assert_eq!(rb_kind(&Ty::Prim(Prim::Bool)), ":bool");
        assert_eq!(rb_kind(&Ty::Enum("Color".into())), ":i32");
        assert_eq!(rb_kind(&opt(Ty::Prim(Prim::U16))), "[:opt, :u16]");
        assert_eq!(rb_kind(&opt(Ty::Enum("Color".into()))), "[:opt, :i32]");
        assert_eq!(rb_kind(&Ty::Prim(Prim::String)), ":string");
        assert_eq!(rb_kind(&list(Ty::Prim(Prim::F32))), "[:slice, :f32]");
        assert_eq!(
            rb_kind(&list(Ty::Prim(Prim::String))),
            "[:buffer, [:list, :string]]"
        );
        assert_eq!(
            rb_kind(&opt(Ty::Interface("Store".into()))),
            "[:object?, Store]"
        );
        assert_eq!(rb_ffi_type(&CType::Opt(Prim::I32)), "OptI32.by_value");
        assert_eq!(rb_ffi_type(&CType::Run), "Run.by_value");
        assert_eq!(
            rb_ffi_type(&CType::Callback("L".into())),
            "Callback.by_value"
        );
        assert_eq!(rb_ffi_type(&CType::Enum("Color".into())), ":int32");
        assert_eq!(rb_ffi_type(&CType::ptr(CType::Error)), ":pointer");
    }
}
