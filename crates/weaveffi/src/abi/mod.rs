//! The stable C ABI runtime: the error struct, memory helpers,
//! reference-counted objects, cancel tokens, callback-interface vtables,
//! iterators, the value-buffer codec, and the async executor hook.
//!
//! Producers rarely name these items. The `#[weaveffi::module]` expansion
//! and [`export_runtime!`](crate::export_runtime) call into them as
//! `::weaveffi::abi::*`, so every `unsafe` pointer operation a producer
//! performs has one audited home. The normative description of the contract
//! this module implements is `docs/src/reference/abi.md` in the WeaveFFI
//! repository.
#![allow(unsafe_code)]

pub mod buffer;
pub mod callback;
pub mod cancel;
pub mod contract;
pub mod convert;
pub mod error;
pub mod iter;
pub mod leak;
pub mod marshal;
pub mod object;
pub mod scalar;
pub mod spawn;

pub use buffer::{
    decode_value, encode_value, BufferDecodeError, BufferReader, BufferValue, BufferWriter,
    ByValue, FixedWidth,
};
pub use callback::{
    callback_ret_buffer, callback_ret_bytes, callback_ret_object, callback_ret_object_opt,
    callback_ret_opt, callback_ret_scalar, callback_ret_slice, callback_ret_text, callback_status,
    callback_status_in, convert_foreign, lift_callback, lift_callback_opt, lift_custom_returned,
    CallbackInterface, ForeignCallback, ForeignError, Vtable, VtableHeader, VtableTooSmall,
    OFF_THREAD_MESSAGE, VTABLE_THREAD_AFFINE,
};
pub use cancel::{
    cancel_token_cancel, cancel_token_create, cancel_token_destroy, cancel_token_is_cancelled,
    AtomicWaker, CancelToken, Cancellable, FfiCancelToken,
};
pub use contract::{contract_compact, contract_len, contract_table, ContractEntry};
pub use convert::{
    adopt_bytes, alloc, bytes_into_raw, free_bytes, lift_byte_slice, lift_bytes, lift_str,
    lift_string, lower_bytes, lower_string, slice_into_raw, RUN_ALIGN,
};
pub use error::{
    boxed_error, error_clear, error_free, error_set, error_set_c, error_set_payload_c, error_store,
    panic_message, ErrorDomain, FfiError, CANCELLED_ERROR_CODE, FOREIGN_ERROR_CODE,
    GENERIC_ERROR_CODE, MARSHAL_ERROR_CODE, PANIC_ERROR_CODE,
};
pub use iter::{iter_destroy, iter_into_raw, iter_next, Iter, IterHandle};
pub use leak::debug_live;
pub use marshal::{
    buffer_run, byte_slots, bytes_run, call_sync, lift_buffer_param, lift_byte_slice_param,
    lift_bytes_param, lift_callback_opt_param, lift_callback_param, lift_custom_buffered,
    lift_custom_param, lift_object_arc_opt_param, lift_object_arc_param, lift_object_opt_param,
    lift_object_param, lift_opt_param, lift_scalar_param, lift_self, lift_self_arc,
    lift_slice_param, lift_slice_vec_param, lift_str_param, lift_text_param, lower_buffer_ret,
    lower_bytes_ret, lower_opt_ret, lower_slice_ret, lower_string_ret, opt_run, opt_slots,
    read_enum, slice_run, string_run, write_enum, Sentinel,
};
pub use object::{
    lower_object, lower_object_opt, object_arc, object_clone, object_destroy, object_from_token,
    object_ref, object_to_token,
};
pub use scalar::{Custom, Scalar, Text};
pub use spawn::{
    block_on, launch_async, run_async, set_spawner, spawn, BoxFuture, CatchUnwind, SpawnError,
    Spawner, SpawnerAlreadySet,
};

/// The revision of the WeaveFFI C ABI this runtime implements.
///
/// Every producer exports it as `{prefix}_abi_version()` (via
/// `weaveffi::export_runtime!()`), and every generated consumer compares it
/// with the revision it was generated against when it loads the library,
/// turning a silent memory-layout mismatch into a clear error.
///
/// The number only changes when the runtime surface (the error layout, the
/// value-buffer encoding, the object or callback-interface conventions, or
/// the set and signatures of the runtime symbols) changes incompatibly. It's
/// independent of the crate version and of the IDL schema version.
///
/// Revision 5 gave the error struct a length-delimited message
/// (`message_ptr`, `message_len`, set with `{prefix}_error_set(err, code,
/// ptr, len)`), passes optional scalars (OptDirect) and numeric lists
/// (Slice) directly instead of through value buffers, allocates every byte
/// run with alignment 8, adds the thread-affine vtable flag, and splits the
/// contract tables' error-domain and callback-interface entries per code
/// and per method, so a revision-5 library can grow without a revision 6.
pub const ABI_VERSION: u32 = 5;
