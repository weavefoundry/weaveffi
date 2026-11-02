//! Value marshalling: the Go expression that moves one value across the
//! boundary in each direction, per its [`Crossing`].
//!
//! Every value is one C value in every position, so the four directions
//! below cover parameters, returns, async results, iterator items, and
//! callback arguments and returns alike:
//!
//! | Crossing | [`lend`] (parameter) | [`take`] (return, result, item) | [`borrow`] (callback argument) | [`hand_over`] (callback return) |
//! |---|---|---|---|---|
//! | scalar, enum | converted | converted | converted | converted |
//! | optional scalar or enum | `wvOptIn*` | `wvOptOut*` | `wvOptOut*` | `wvOptIn*` |
//! | string, bytes, typed array | a view of the Go memory | copied, then `run_free` | copied | copied into `{p}_alloc` |
//! | value buffer | encoded, then viewed | decoded, then `run_free` | decoded | encoded into `{p}_alloc` |
//! | object | the wrapper's pointer, borrowed | adopted | adopted | a fresh reference (`share`) |

use weaveffi_model::abi::{CType, Position};
use weaveffi_model::ty::{Prim, Ty};

use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use crate::targets::go::codec::{read_fn, write_fn};
use crate::targets::go::names::pascal;
use crate::targets::go::types::{cgo_type, go_type, prim_type};
use crate::targets::go::Ctx;

/// The C type of `ty` in `position`, in cgo spelling.
pub(crate) fn cgo_value(ctx: &Ctx, ty: &Ty, position: Position) -> String {
    cgo_type(&CType::of(ty, position), ctx.prefix)
}

/// The runtime's optional-scalar converters for `p` (`wvOptInI64`,
/// `wvOptOutI64`) take this stem.
fn opt_stem(p: Prim) -> &'static str {
    p.pascal()
}

/// Emit the staging a parameter `name` of type `ty` needs and return its C
/// argument: a scalar converted, an optional joined into its C struct, a
/// string, bytes, or typed array lent as a view of the Go memory (cgo
/// keeps it in place for the call), a buffered value encoded first, and an
/// object's pointer borrowed for the call like a receiver's (a nil
/// `Interface?` passes null).
pub(crate) fn lend(w: &mut CodeWriter, ctx: &Ctx, ty: &Ty, name: &str, c: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(_) | Crossing::Enum(_) => {
            format!("{}({name})", cgo_value(ctx, ty, Position::Param))
        }
        Crossing::OptScalar(p) => format!("wvOptIn{}({name})", opt_stem(p)),
        Crossing::OptEnum(_) => format!("wvOptInI32((*int32)({name}))"),
        Crossing::Utf8 => format!("wvStrRun({name})"),
        Crossing::Bytes => format!("wvBytesRun({name})"),
        Crossing::Slice(_) => format!("wvSliceRun({name})"),
        Crossing::Buffer(ty) => format!("wvBytesRun(wvEncode({name}, {}))", write_fn(ty)),
        Crossing::Object { nullable, .. } => {
            if nullable {
                w.line(format!("var {c} {}", cgo_value(ctx, ty, Position::Param)));
                w.block(format!("if {name} != nil {{"), "}", |w| {
                    w.line(format!("{c} = {name}.native()"));
                    w.line(format!("defer {name}.release()"));
                });
            } else {
                w.line(format!("{c} := {name}.native()"));
                w.line(format!("defer {name}.release()"));
            }
            c.to_string()
        }
    }
}

/// The Go expression receiving the C value `c` of type `ty` that the
/// library hands over (a return, an async result, or an iterator item),
/// taking ownership: a run is copied or decoded and then released, and an
/// object is adopted (a null `Interface?` adopts to nil).
pub(crate) fn take(ty: &Ty, c: &str) -> String {
    receive(ty, c, true)
}

/// The Go expression receiving the C value `c` of type `ty` that the
/// library lends a callback method: like [`take`], but a run is only
/// copied or decoded, since the library keeps it. An object still moves
/// one strong reference, which the new wrapper adopts.
pub(crate) fn borrow(ty: &Ty, c: &str) -> String {
    receive(ty, c, false)
}

fn receive(ty: &Ty, c: &str, owned: bool) -> String {
    let how = if owned { "Take" } else { "Borrow" };
    match crossing(ty) {
        Crossing::Scalar(_) | Crossing::Enum(_) => format!("{}({c})", go_type(ty)),
        Crossing::OptScalar(p) => format!("wvOptOut{}({c})", opt_stem(p)),
        Crossing::OptEnum(e) => format!("(*{})(wvOptOutI32({c}))", pascal(e)),
        Crossing::Utf8 => format!("wv{how}String({c})"),
        Crossing::Bytes => format!("wv{how}Bytes({c})"),
        Crossing::Slice(p) => format!("wv{how}Slice[{}]({c})", prim_type(p)),
        Crossing::Buffer(ty) if owned => format!("wvDecode({c}, {})", read_fn(ty)),
        Crossing::Buffer(ty) => format!("wvDecodeBorrowed({c}, {})", read_fn(ty)),
        Crossing::Object { interface, .. } => format!("wvAdopt{}({c})", pascal(interface)),
    }
}

/// The C value a callback method returns for the Go value `v` of type
/// `ty`, which the library adopts: a scalar converted, an optional joined,
/// a string, bytes, typed array, or encoded buffer copied into a run from
/// the library's allocator, and an object as a fresh strong reference (nil
/// stays null).
pub(crate) fn hand_over(ctx: &Ctx, ty: &Ty, v: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(_) | Crossing::Enum(_) => {
            format!("{}({v})", cgo_value(ctx, ty, Position::Transfer))
        }
        Crossing::OptScalar(p) => format!("wvOptIn{}({v})", opt_stem(p)),
        Crossing::OptEnum(_) => format!("wvOptInI32((*int32)({v}))"),
        Crossing::Utf8 => format!("wvAllocString({v})"),
        Crossing::Bytes | Crossing::Slice(_) => format!("wvAllocSlice({v})"),
        Crossing::Buffer(ty) => format!("wvAllocSlice(wvEncode({v}, {}))", write_fn(ty)),
        Crossing::Object { .. } => format!("{v}.share()"),
    }
}
