//! Hand-annotated form of the kitchen-sink IDL, used by the
//! `extract_roundtrip::roundtrip_kitchen_sink` test. Each item carries the
//! WeaveFFI marker attributes that `weaveffi extract` recognizes so the
//! file round-trips back to the original IR shape.
//!
//! The file is parse-only; it is never compiled. The marker attributes
//! (`#[weaveffi::*]`) resolve by their final path segment, so the bare
//! syntactic markers here match the same names the proc-macro re-emits.
//!
//! Round-trip gaps documented in `docs/src/guides/extract.md`:
//!   * The original `stream_items` IDL function (an `iter<T>` return) is
//!     omitted from this fixture.
//!   * Struct field `default:` values: dropped.
//!   * Standalone `since:` without `#[deprecated]`: dropped, so `new_op`
//!     loses its `since: "0.3.0"`.
//!   * An error code's yml `doc:` cannot be expressed separately from its
//!     `message:` in Rust (the message is the first doc line), so the
//!     round-trip test compares codes by name/code/message only.

#![allow(dead_code)]
#![allow(unused_attributes)]
#![allow(unused_variables)]

#[weaveffi::module]
mod shared {
    /// A reusable token referenced across modules
    #[weaveffi::record]
    struct Token {
        id: i64,
        label: String,
    }

    /// Trivial cross-module helper
    #[weaveffi::export]
    fn ping() -> String {
        String::new()
    }
}

#[weaveffi::module]
mod kitchen {
    /// Task priority levels
    #[weaveffi::enumeration]
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
    #[weaveffi::record]
    #[weaveffi::builder]
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
    #[weaveffi::callback]
    fn OnReady(code: i32, msg: String) {}

    /// Subscribe to OnReady events
    #[weaveffi::listener(event = "OnReady")]
    fn ready_listener() {}

    #[weaveffi::error]
    enum KitchenErrors {
        /// Item not found
        NotFound = 1,
        /// Invalid input
        InvalidInput = 2,
    }

    /// A pocket-sized interface exercising the object surface
    #[weaveffi::interface]
    struct Gadget {
        id: i64,
    }

    impl Gadget {
        /// Create a gadget with the given id
        pub fn new(id: i64) -> Self {
            Gadget { id }
        }

        /// Render the gadget as a human-readable string
        pub fn describe(&self) -> String {
            format!("gadget {}", self.id)
        }

        /// Poke the gadget a number of times, failing on a negative count
        pub fn poke(&self, times: i32) -> Result<i32, KitchenErrors> {
            if times < 0 {
                return Err(KitchenErrors::InvalidInput);
            }
            Ok(times)
        }

        /// The gadget subsystem version string
        pub fn version() -> String {
            String::new()
        }
    }

    /// Identity for bool
    #[weaveffi::export]
    fn bool_id(x: bool) -> bool {
        x
    }

    #[weaveffi::export]
    fn i32_id(x: i32) -> i32 {
        x
    }

    #[weaveffi::export]
    fn u32_id(x: u32) -> u32 {
        x
    }

    #[weaveffi::export]
    fn i64_id(x: i64) -> i64 {
        x
    }

    #[weaveffi::export]
    fn f64_id(x: f64) -> f64 {
        x
    }

    #[weaveffi::export]
    fn echo_string(s: String) -> String {
        s
    }

    #[weaveffi::export]
    fn echo_bytes(b: Vec<u8>) -> Vec<u8> {
        b
    }

    /// Borrowed string parameter
    #[weaveffi::export]
    fn echo_borrowed_str(s: &str) -> String {
        s.to_string()
    }

    /// Borrowed bytes parameter
    #[weaveffi::export]
    fn echo_borrowed_bytes(b: &[u8]) -> Vec<u8> {
        b.to_vec()
    }

    /// Returns an opaque handle
    #[weaveffi::export]
    fn open_handle() -> u64 {
        0
    }

    /// Returns a typed handle
    #[weaveffi::export]
    fn open_typed_handle() -> *mut Token {
        std::ptr::null_mut()
    }

    /// Optional struct return
    #[weaveffi::export]
    fn maybe_item(id: i64) -> Result<Option<Item>, KitchenErrors> {
        Ok(None)
    }

    /// List of structs
    #[weaveffi::export]
    fn list_items() -> Vec<Item> {
        Vec::new()
    }

    /// Map return type
    #[weaveffi::export]
    fn get_attrs() -> HashMap<String, i32> {
        HashMap::new()
    }

    /// Returns the shared Token type from another module
    #[weaveffi::export]
    fn cross_module_token() -> Token {
        todo!()
    }

    /// Async operation
    #[weaveffi::export]
    async fn do_async(input: String) -> String {
        input
    }

    /// Cancellable async operation
    #[weaveffi::export]
    #[weaveffi::cancellable]
    async fn do_cancellable(input: String) -> String {
        input
    }

    /// Legacy operation kept for compatibility
    #[weaveffi::export]
    #[deprecated(since = "0.1.0", note = "Use new_op instead")]
    fn legacy_op() -> i32 {
        0
    }

    /// Replacement for legacy_op
    #[weaveffi::export]
    fn new_op() -> i32 {
        0
    }

    #[weaveffi::module]
    mod nested {
        /// Trivial nested-module function
        #[weaveffi::export]
        fn hello() -> String {
            String::new()
        }
    }
}
