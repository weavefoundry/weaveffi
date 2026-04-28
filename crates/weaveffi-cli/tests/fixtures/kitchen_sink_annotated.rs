//! Hand-annotated form of the kitchen-sink IDL, used by the
//! `extract_roundtrip::roundtrip_kitchen_sink` test. Each item carries the
//! WeaveFFI marker attributes that `weaveffi extract` recognises so the
//! file round-trips back to the original IR shape.
//!
//! The file is parse-only; it is never compiled. The marker attributes
//! (`#[weaveffi_*]`) are not real proc-macros, just syntactic markers that
//! the extractor matches by name.
//!
//! Round-trip gaps documented in `docs/src/guides/extract.md`:
//!   * `iter<T>` returns: no equivalent Rust syntax, so the original
//!     `stream_items` IDL function is omitted from this fixture.
//!   * Error domains: not derivable from Rust, so `KitchenErrors` is
//!     omitted.
//!   * Struct field `default:` values: dropped.
//!   * Standalone `since:` without `#[deprecated]`: dropped, so `new_op`
//!     loses its `since: "0.3.0"`.

#![allow(dead_code)]
#![allow(unused_attributes)]
#![allow(unused_variables)]

mod shared {
    /// A reusable token referenced across modules
    #[weaveffi_struct]
    struct Token {
        id: i64,
        label: String,
    }

    /// Trivial cross-module helper
    #[weaveffi_export]
    fn ping() -> String {
        String::new()
    }
}

mod kitchen {
    /// Task priority levels
    #[weaveffi_enum]
    #[repr(i32)]
    enum Priority {
        /// Low priority
        Low = 0,
        /// Normal priority
        Normal = 1,
        /// High priority
        High = 2,
    }

    /// Kitchen sink struct exercising every field feature
    #[weaveffi_struct]
    #[weaveffi_builder]
    struct Item {
        /// Stable identifier
        id: i64,
        /// Display name
        name: String,
        /// Initial count
        count: i32,
        /// Whether the item is enabled
        enabled: bool,
        /// Scaling ratio
        ratio: f64,
        nick: u32,
        payload: Vec<u8>,
        tags: Vec<String>,
        attrs: HashMap<String, String>,
        parent: Option<i64>,
        /// Cross-module struct reference
        token: Token,
        priority: Priority,
    }

    /// Fires when an item is ready
    #[weaveffi_callback]
    fn OnReady(code: i32, msg: String) {}

    /// Subscribe to OnReady events
    #[weaveffi_listener(event_callback = "OnReady")]
    fn ready_listener() {}

    /// Identity for bool
    #[weaveffi_export]
    fn bool_id(x: bool) -> bool {
        x
    }

    #[weaveffi_export]
    fn i32_id(x: i32) -> i32 {
        x
    }

    #[weaveffi_export]
    fn u32_id(x: u32) -> u32 {
        x
    }

    #[weaveffi_export]
    fn i64_id(x: i64) -> i64 {
        x
    }

    #[weaveffi_export]
    fn f64_id(x: f64) -> f64 {
        x
    }

    #[weaveffi_export]
    fn echo_string(s: String) -> String {
        s
    }

    #[weaveffi_export]
    fn echo_bytes(b: Vec<u8>) -> Vec<u8> {
        b
    }

    /// Borrowed string parameter
    #[weaveffi_export]
    fn echo_borrowed_str(s: &str) -> String {
        s.to_string()
    }

    /// Borrowed bytes parameter
    #[weaveffi_export]
    fn echo_borrowed_bytes(b: &[u8]) -> Vec<u8> {
        b.to_vec()
    }

    /// Returns an opaque handle
    #[weaveffi_export]
    fn open_handle() -> u64 {
        0
    }

    /// Returns a typed handle
    #[weaveffi_export]
    fn open_typed_handle() -> *mut Token {
        std::ptr::null_mut()
    }

    /// Optional struct return
    #[weaveffi_export]
    fn maybe_item(id: i64) -> Option<Item> {
        None
    }

    /// List of structs
    #[weaveffi_export]
    fn list_items() -> Vec<Item> {
        Vec::new()
    }

    /// Map return type
    #[weaveffi_export]
    fn get_attrs() -> HashMap<String, i32> {
        HashMap::new()
    }

    /// Returns the shared Token type from another module
    #[weaveffi_export]
    fn cross_module_token() -> Token {
        todo!()
    }

    /// Async operation
    #[weaveffi_export]
    #[weaveffi_async]
    fn do_async(input: String) -> String {
        input
    }

    /// Cancellable async operation
    #[weaveffi_export]
    #[weaveffi_async]
    #[weaveffi_cancellable]
    fn do_cancellable(input: String) -> String {
        input
    }

    /// Legacy operation kept for compatibility
    #[weaveffi_export]
    #[deprecated(since = "0.1.0", note = "Use new_op instead")]
    fn legacy_op() -> i32 {
        0
    }

    /// Replacement for legacy_op
    #[weaveffi_export]
    fn new_op() -> i32 {
        0
    }

    mod nested {
        /// Trivial nested-module function
        #[weaveffi_export]
        fn hello() -> String {
            String::new()
        }
    }
}
