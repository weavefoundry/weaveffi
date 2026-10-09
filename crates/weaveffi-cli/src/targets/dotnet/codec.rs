//! Value-buffer codec and value equality expressions.
//!
//! Nothing here is monomorphized per composite type: optionals, lists, and
//! maps go through the runtime's generic `FfiBufferWriter.WriteList` /
//! `FfiBufferReader.ReadList` (and friends), handed a `static` lambda for
//! the element, so `[{string:[i32]}]` is three nested calls rather than a
//! generated method per shape. Records and rich enums carry their own
//! `WriteTo`/`ReadFrom` pair. An object token carries one strong reference:
//! writing one clones the wrapper's handle, and reading one adopts the
//! pointer into a new wrapper.

use weaveffi_model::ty::{Prim, Ty};

use crate::targets::dotnet::types::{cs_type, is_value_type, Cx};

/// The C# expression (a `void` call) writing `value`, the surface value of
/// `ty`, into the writer `writer`. `depth` numbers the lambda parameters of
/// nested composites so they never shadow each other.
pub(crate) fn write_call(ty: &Ty, writer: &str, value: &str, depth: usize) -> String {
    match ty {
        Ty::Prim(p) => format!("{writer}.Write{}({value})", p.pascal()),
        Ty::Enum(_) => format!("{writer}.WriteI32((int){value})"),
        Ty::Interface(_) => format!("{writer}.WriteObject({value}.CloneHandle())"),
        Ty::Record(_) | Ty::RichEnum(_) => format!("{value}.WriteTo({writer})"),
        Ty::Optional(inner) => {
            let method = if is_value_type(inner) {
                "WriteOptionalValue"
            } else {
                "WriteOptional"
            };
            format!("{writer}.{method}({value}, {})", write_lambda(inner, depth))
        }
        Ty::List(inner) => format!(
            "{writer}.WriteList({value}, {})",
            write_lambda(inner, depth)
        ),
        Ty::Map(k, v) => format!(
            "{writer}.WriteMap({value}, {}, {})",
            write_lambda(k, depth),
            write_lambda(v, depth)
        ),
    }
}

/// A `static` lambda writing one value of `ty`: the element writer handed to
/// the runtime's generic composite methods.
pub(crate) fn write_lambda(ty: &Ty, depth: usize) -> String {
    let (w, v) = (format!("w{depth}"), format!("v{depth}"));
    format!("static ({w}, {v}) => {}", write_call(ty, &w, &v, depth + 1))
}

/// The C# expression reading a value of `ty` from the reader `reader`; the
/// inverse of [`write_call`]. An object token is adopted into a new
/// wrapper, which owes the reference's release.
pub(crate) fn read_expr(cx: Cx<'_>, ty: &Ty, reader: &str, depth: usize) -> String {
    match ty {
        Ty::Prim(p) => format!("{reader}.Read{}()", p.pascal()),
        Ty::Enum(name) => format!("({}){reader}.ReadI32()", cx.ty(name)),
        Ty::Interface(name) => format!("{}.Adopt({reader}.ReadObject())", cx.ty(name)),
        Ty::Record(name) | Ty::RichEnum(name) => format!("{}.ReadFrom({reader})", cx.ty(name)),
        Ty::Optional(inner) => {
            let method = if is_value_type(inner) {
                "ReadOptionalValue"
            } else {
                "ReadOptional"
            };
            format!("{reader}.{method}({})", read_lambda(cx, inner, depth))
        }
        Ty::List(inner) => format!("{reader}.ReadList({})", read_lambda(cx, inner, depth)),
        // A dictionary isn't covariant in its value type, so the reader
        // builds it with the surface types spelled out.
        Ty::Map(k, v) => format!(
            "{reader}.ReadMap<{}, {}>({}, {})",
            cs_type(k),
            cs_type(v),
            read_lambda(cx, k, depth),
            read_lambda(cx, v, depth)
        ),
    }
}

/// A `static` lambda reading one value of `ty`.
pub(crate) fn read_lambda(cx: Cx<'_>, ty: &Ty, depth: usize) -> String {
    let r = format!("r{depth}");
    format!("static {r} => {}", read_expr(cx, ty, &r, depth + 1))
}

/// True when the surface type of `ty` compares by reference in C# (a byte
/// array, a list, or a map, possibly optional), so a record's value
/// equality has to compare its contents explicitly.
pub(crate) fn needs_deep_equality(ty: &Ty) -> bool {
    match ty {
        Ty::Prim(Prim::Bytes) | Ty::List(_) | Ty::Map(..) => true,
        Ty::Optional(inner) => needs_deep_equality(inner),
        _ => false,
    }
}

/// The C# expression comparing two values of `ty` by value: contents for
/// byte arrays, lists, and maps, default equality for everything else.
pub(crate) fn equal_expr(ty: &Ty, a: &str, b: &str, depth: usize) -> String {
    match ty {
        Ty::Prim(Prim::Bytes) => format!("FfiEquality.Bytes({a}, {b})"),
        Ty::Optional(inner) if needs_deep_equality(inner) => equal_expr(inner, a, b, depth),
        Ty::List(inner) => match equal_lambda(inner, depth) {
            Some(eq) => format!("FfiEquality.Lists({a}, {b}, {eq})"),
            None => format!("FfiEquality.Lists({a}, {b})"),
        },
        Ty::Map(_, v) => match equal_lambda(v, depth) {
            Some(eq) => format!("FfiEquality.Maps({a}, {b}, {eq})"),
            None => format!("FfiEquality.Maps({a}, {b})"),
        },
        _ => format!("FfiEquality.Equal({a}, {b})"),
    }
}

/// The element comparer of a collection whose elements need content
/// equality, or `None` when default equality does.
fn equal_lambda(ty: &Ty, depth: usize) -> Option<String> {
    needs_deep_equality(ty).then(|| {
        let (x, y) = (format!("x{depth}"), format!("y{depth}"));
        format!("static ({x}, {y}) => {}", equal_expr(ty, &x, &y, depth + 1))
    })
}

/// The C# expression a record's `GetHashCode` adds for one field: the value
/// itself, or a hash consistent with [`equal_expr`] for collections.
pub(crate) fn hash_expr(ty: &Ty, value: &str) -> String {
    match ty {
        Ty::Prim(Prim::Bytes) => format!("FfiEquality.BytesHash({value})"),
        Ty::List(_) => format!("FfiEquality.ListHash({value})"),
        Ty::Map(..) => format!("FfiEquality.MapHash({value})"),
        Ty::Optional(inner) if needs_deep_equality(inner) => hash_expr(inner, value),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(t: Ty) -> Ty {
        Ty::List(Box::new(t))
    }

    #[test]
    fn nested_composites_are_nested_generic_calls() {
        let cx = Cx {
            ns: "Kv",
            base: "NativeException",
            bug: "NativeBugException",
        };
        let ty = Ty::Map(
            Box::new(Ty::Prim(Prim::String)),
            Box::new(list(Ty::Optional(Box::new(Ty::Prim(Prim::I32))))),
        );
        assert_eq!(
            write_call(&ty, "writer", "Value", 0),
            "writer.WriteMap(Value, static (w0, v0) => w0.WriteString(v0), \
             static (w0, v0) => w0.WriteList(v0, static (w1, v1) => \
             w1.WriteOptionalValue(v1, static (w2, v2) => w2.WriteI32(v2))))"
        );
        assert_eq!(
            read_expr(cx, &ty, "reader", 0),
            "reader.ReadMap<string, IReadOnlyList<int?>>(static r0 => r0.ReadString(), \
             static r0 => r0.ReadList(static r1 => r1.ReadOptionalValue(static r2 => r2.ReadI32())))"
        );
    }

    #[test]
    fn equality_compares_collection_contents() {
        let bytes = Ty::Prim(Prim::Bytes);
        assert_eq!(
            equal_expr(&list(bytes.clone()), "A", "o.A", 0),
            "FfiEquality.Lists(A, o.A, static (x0, y0) => FfiEquality.Bytes(x0, y0))"
        );
        assert_eq!(
            equal_expr(&list(Ty::Prim(Prim::I32)), "A", "o.A", 0),
            "FfiEquality.Lists(A, o.A)"
        );
        assert_eq!(
            equal_expr(&Ty::Prim(Prim::F64), "A", "o.A", 0),
            "FfiEquality.Equal(A, o.A)"
        );
        assert!(!needs_deep_equality(&Ty::Optional(Box::new(Ty::Prim(
            Prim::I64
        )))));
        assert_eq!(
            hash_expr(&Ty::Optional(Box::new(bytes)), "B"),
            "FfiEquality.BytesHash(B)"
        );
    }
}
