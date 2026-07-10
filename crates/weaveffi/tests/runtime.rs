//! End-to-end runtime tests for the `#[weaveffi::module]` expansion.
//!
//! Each test defines a module with the macro, then calls the generated
//! `#[no_mangle] extern "C"` thunks directly (by their Rust path) and checks
//! that arguments lift, results lower, and errors flow through `out_err` the
//! way the C ABI promises. This is the executable proof that the generated glue
//! matches the calling convention every language binding expects.

#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use weaveffi::abi::{self, c_ptr_to_string, free_string, string_to_c_ptr, weaveffi_error};

#[weaveffi::module]
pub mod demo {
    /// The demo module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum DemoError {
        /// division by zero
        DivisionByZero = 100,
    }

    /// A C-style enum that crosses the ABI as its `i32` discriminant.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Color {
        /// Red.
        Red = 0,
        /// Green.
        Green = 1,
        /// Blue.
        Blue = 2,
    }

    /// A by-value record with scalar, string, optional, and enum fields.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Point {
        /// The x coordinate.
        pub x: i32,
        /// A human-readable label.
        pub label: String,
        /// An optional nickname.
        pub nickname: Option<String>,
        /// The point's color.
        pub color: Color,
    }

    /// Add two integers.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    /// Divide, surfacing division by zero as a domain error.
    #[weaveffi::export]
    pub fn checked_div(a: i32, b: i32) -> Result<i32, DemoError> {
        if b == 0 {
            return Err(DemoError::DivisionByZero);
        }
        Ok(a / b)
    }

    /// Greet by name (owned string in, owned string out).
    #[weaveffi::export]
    pub fn greet(name: String) -> String {
        format!("hi {name}")
    }

    /// Borrow a string slice and report its length.
    #[weaveffi::export]
    pub fn str_len(text: &str) -> i32 {
        text.chars().count() as i32
    }

    /// Return an optional string depending on the flag.
    #[weaveffi::export]
    pub fn maybe_name(present: bool) -> Option<String> {
        present.then(|| "present".to_string())
    }

    /// Sum a list of scalars.
    #[weaveffi::export]
    pub fn sum(xs: Vec<i32>) -> i32 {
        xs.iter().sum()
    }

    /// Join a list of strings with a comma.
    #[weaveffi::export]
    pub fn join(parts: Vec<String>) -> String {
        parts.join(",")
    }

    /// Count bytes in an owned buffer.
    #[weaveffi::export]
    pub fn byte_count(data: Vec<u8>) -> i32 {
        data.len() as i32
    }

    /// Build a point by value (returned as an owning pointer).
    #[weaveffi::export]
    pub fn make_point(x: i32) -> Point {
        Point {
            x,
            label: "origin".to_string(),
            nickname: None,
            color: Color::Green,
        }
    }
}

#[weaveffi::module]
pub mod warehouse {
    /// A record owned by the `warehouse` module.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct Crate {
        /// Stable identifier.
        pub id: i64,
        /// Display label.
        pub label: String,
    }

    /// Build a crate by value.
    #[weaveffi::export]
    pub fn make_crate(id: i64, label: String) -> Crate {
        Crate { id, label }
    }
}

#[weaveffi::module]
pub mod dispatch {
    // A struct declared in a *sibling* top-level module. The macro expands each
    // module in isolation, so this exercises cross-module type resolution: the
    // thunk must accept/return the producer's real `Crate` type as an opaque
    // pointer without the per-module expansion rejecting it as unknown.
    use super::warehouse::Crate;

    /// Read a sibling-module record's id (struct parameter by value).
    #[weaveffi::export]
    pub fn crate_id(item: Crate) -> i64 {
        item.id
    }

    /// Return a relabeled copy (sibling-module struct in and out).
    #[weaveffi::export]
    pub fn relabel(item: Crate, label: String) -> Crate {
        Crate { id: item.id, label }
    }
}

#[weaveffi::module]
pub mod maps {
    use std::collections::BTreeMap;

    /// Double every value in a string-keyed map (map in, map out).
    #[weaveffi::export]
    pub fn double_scores(scores: BTreeMap<String, i32>) -> BTreeMap<String, i32> {
        scores.into_iter().map(|(k, v)| (k, v * 2)).collect()
    }

    /// Sum a map's values (map parameter, scalar return).
    #[weaveffi::export]
    pub fn total(scores: BTreeMap<String, i32>) -> i32 {
        scores.values().sum()
    }
}

#[weaveffi::module]
pub mod build {
    /// A record that also exposes a fluent builder.
    #[weaveffi::record]
    #[weaveffi::builder]
    #[derive(Clone)]
    pub struct Widget {
        /// Required display name.
        pub name: String,
        /// Quantity on hand.
        pub qty: i32,
        /// Optional shelf note.
        pub note: Option<String>,
    }
}

#[weaveffi::module]
pub mod geom {
    /// An algebraic shape: variants carry associated data, so it crosses the
    /// ABI as an opaque object (tag reader + per-variant constructors/getters).
    #[weaveffi::enumeration]
    #[derive(Clone)]
    pub enum Shape {
        /// The empty shape (a unit variant, tag 0).
        Empty,
        /// A circle with a radius (tag 1).
        Circle {
            /// The radius.
            radius: f64,
        },
        /// A labeled count (tag 2, by declaration order).
        Labeled {
            /// The label text.
            label: String,
            /// The count.
            count: u8,
        },
    }

    /// Describe a shape (rich enum borrowed in, owned string out).
    #[weaveffi::export]
    pub fn describe(shape: &Shape) -> String {
        match shape {
            Shape::Empty => "empty".to_string(),
            Shape::Circle { radius } => format!("circle({radius})"),
            Shape::Labeled { label, count } => format!("{label}x{count}"),
        }
    }
}

#[weaveffi::module]
pub mod stream {
    /// Yield `count` greetings lazily as an `iter<String>`.
    #[weaveffi::export]
    pub fn greetings(count: i32) -> weaveffi::Iter<String> {
        weaveffi::Iter::new((0..count).map(|i| format!("hi {i}")))
    }

    /// Yield the squares `0..count` lazily as an `iter<i32>`.
    #[weaveffi::export]
    pub fn squares(count: i32) -> weaveffi::Iter<i32> {
        weaveffi::Iter::new((0..count).map(|i| i * i))
    }
}

#[weaveffi::module]
pub mod bus {
    /// Invoked for every published message with its text and weight.
    #[weaveffi::callback]
    #[allow(dead_code, unused_variables)]
    fn on_message(text: String, weight: i32) {}

    /// The set of subscribers to published messages.
    #[weaveffi::listener(event = "on_message")]
    #[allow(dead_code)]
    fn subscribers() {}

    /// Publish a message, fanning it out to every registered subscriber.
    #[weaveffi::export]
    pub fn publish(text: String, weight: i32) {
        emit_subscribers(&text, weight);
    }
}

#[weaveffi::module]
pub mod tasks {
    /// The task module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum TaskError {
        /// arithmetic overflow
        Overflow = 1,
    }

    /// The by-value result an async task completes with.
    #[weaveffi::record]
    #[derive(Clone)]
    pub struct TaskResult {
        /// The assigned task id.
        pub id: i64,
        /// A human-readable completion message.
        pub value: String,
    }

    /// Run a named task asynchronously, completing with a `TaskResult`.
    #[weaveffi::export]
    pub async fn run_task(name: String) -> TaskResult {
        TaskResult {
            id: 7,
            value: format!("done: {name}"),
        }
    }

    /// Add two integers asynchronously, failing on overflow.
    #[weaveffi::export]
    pub async fn checked_add(a: i32, b: i32) -> Result<i32, TaskError> {
        a.checked_add(b).ok_or(TaskError::Overflow)
    }
}

fn ok_err() -> weaveffi_error {
    weaveffi_error::default()
}

#[test]
fn scalar_call_sets_ok() {
    let mut err = ok_err();
    let r = demo::weaveffi_demo_add(2, 40, &mut err);
    assert_eq!(r, 42);
    assert_eq!(err.code, 0);
    assert!(err.message.is_null());
}

#[test]
fn fallible_ok_and_err_paths() {
    let mut err = ok_err();
    assert_eq!(demo::weaveffi_demo_checked_div(10, 2, &mut err), 5);
    assert_eq!(err.code, 0);

    let r = demo::weaveffi_demo_checked_div(1, 0, &mut err);
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(
        err.code, 100,
        "domain code from the #[weaveffi::error] enum"
    );
    assert_eq!(c_ptr_to_string(err.message).unwrap(), "division by zero");
    abi::error_clear(&mut err);
}

#[test]
fn owned_string_roundtrip() {
    let mut err = ok_err();
    let input = string_to_c_ptr("alice");
    let out = demo::weaveffi_demo_greet(input, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(c_ptr_to_string(out).unwrap(), "hi alice");
    free_string(out);
    free_string(input);
}

#[test]
fn borrowed_str_param() {
    let mut err = ok_err();
    let input = string_to_c_ptr("héllo");
    assert_eq!(demo::weaveffi_demo_str_len(input, &mut err), 5);
    free_string(input);
}

#[test]
fn optional_string_return() {
    let mut err = ok_err();
    let some = demo::weaveffi_demo_maybe_name(true, &mut err);
    assert_eq!(c_ptr_to_string(some).unwrap(), "present");
    free_string(some);

    let none = demo::weaveffi_demo_maybe_name(false, &mut err);
    assert!(none.is_null());
}

#[test]
fn scalar_list_param() {
    let mut err = ok_err();
    let xs = [3i32, 4, 5];
    let total = demo::weaveffi_demo_sum(xs.as_ptr(), xs.len(), &mut err);
    assert_eq!(total, 12);
}

#[test]
fn string_list_param() {
    let mut err = ok_err();
    let parts = ["a", "b", "c"];
    let ptrs: Vec<*const c_char> = parts.iter().map(string_to_c_ptr).collect();
    let out = demo::weaveffi_demo_join(ptrs.as_ptr(), ptrs.len(), &mut err);
    assert_eq!(c_ptr_to_string(out).unwrap(), "a,b,c");
    free_string(out);
    for p in ptrs {
        free_string(p);
    }
}

#[test]
fn byte_buffer_param() {
    let mut err = ok_err();
    let data = [1u8, 2, 3, 4, 5];
    assert_eq!(
        demo::weaveffi_demo_byte_count(data.as_ptr(), data.len(), &mut err),
        5
    );
}

#[test]
fn record_create_get_destroy() {
    let mut err = ok_err();
    let label = string_to_c_ptr("corner");
    let nickname = string_to_c_ptr("nw");
    // create(x, label, nickname, color, out_err)
    let p = demo::weaveffi_demo_Point_create(7, label, nickname, 2, &mut err);
    assert_eq!(err.code, 0);
    assert!(!p.is_null());
    free_string(label);
    free_string(nickname);

    assert_eq!(demo::weaveffi_demo_Point_get_x(p), 7);
    assert_eq!(demo::weaveffi_demo_Point_get_color(p), 2);
    let got_label = demo::weaveffi_demo_Point_get_label(p);
    assert_eq!(c_ptr_to_string(got_label).unwrap(), "corner");
    free_string(got_label);
    let got_nick = demo::weaveffi_demo_Point_get_nickname(p);
    assert_eq!(c_ptr_to_string(got_nick).unwrap(), "nw");
    free_string(got_nick);

    demo::weaveffi_demo_Point_destroy(p);
}

#[test]
fn record_optional_field_null() {
    let mut err = ok_err();
    let label = string_to_c_ptr("solo");
    // nickname is null -> None
    let p = demo::weaveffi_demo_Point_create(1, label, std::ptr::null(), 0, &mut err);
    assert!(!p.is_null());
    free_string(label);

    let got_nick = demo::weaveffi_demo_Point_get_nickname(p);
    assert!(got_nick.is_null(), "None lowers to a null string pointer");
    demo::weaveffi_demo_Point_destroy(p);
}

#[test]
fn struct_return_by_value() {
    let mut err = ok_err();
    let p = demo::weaveffi_demo_make_point(99, &mut err);
    assert!(!p.is_null());
    assert_eq!(demo::weaveffi_demo_Point_get_x(p), 99);
    let label = demo::weaveffi_demo_Point_get_label(p);
    assert_eq!(c_ptr_to_string(label).unwrap(), "origin");
    free_string(label);
    demo::weaveffi_demo_Point_destroy(p);
}

#[test]
fn cross_module_struct_param_and_return() {
    let mut err = ok_err();
    let label = string_to_c_ptr("widget");
    let c = warehouse::weaveffi_warehouse_make_crate(7, label, &mut err);
    assert_eq!(err.code, 0);
    assert!(!c.is_null());
    free_string(label);

    // `dispatch::crate_id` accepts `warehouse::Crate` as an opaque pointer.
    let id = dispatch::weaveffi_dispatch_crate_id(c, &mut err);
    assert_eq!(id, 7);
    assert_eq!(err.code, 0);

    // `dispatch::relabel` returns a fresh `warehouse::Crate`, freed by the
    // owner module's destructor (same Rust type, same allocation).
    let new_label = string_to_c_ptr("gadget");
    let c2 = dispatch::weaveffi_dispatch_relabel(c, new_label, &mut err);
    assert!(!c2.is_null());
    free_string(new_label);
    let got = warehouse::weaveffi_warehouse_Crate_get_label(c2);
    assert_eq!(c_ptr_to_string(got).unwrap(), "gadget");
    free_string(got);

    warehouse::weaveffi_warehouse_Crate_destroy(c);
    warehouse::weaveffi_warehouse_Crate_destroy(c2);
}

#[test]
fn map_param_and_return() {
    let mut err = ok_err();
    // Pass {"b": 1, "a": 2} as parallel key/value arrays (unsorted on input).
    let kb = string_to_c_ptr("b");
    let ka = string_to_c_ptr("a");
    let keys: [*const c_char; 2] = [kb, ka];
    let values: [i32; 2] = [1, 2];

    let mut out_keys: *mut *const c_char = std::ptr::null_mut();
    let mut out_values: *mut i32 = std::ptr::null_mut();
    let mut out_len: usize = 0;
    maps::weaveffi_maps_double_scores(
        keys.as_ptr(),
        values.as_ptr(),
        2,
        &mut out_keys,
        &mut out_values,
        &mut out_len,
        &mut err,
    );
    assert_eq!(err.code, 0);
    assert_eq!(out_len, 2);

    // The BTreeMap return is sorted: a, b with doubled values 4, 2.
    let got_keys: Vec<String> = (0..out_len)
        .map(|i| c_ptr_to_string(unsafe { *out_keys.add(i) }).unwrap())
        .collect();
    let got_vals: Vec<i32> = (0..out_len)
        .map(|i| unsafe { *out_values.add(i) })
        .collect();
    assert_eq!(got_keys, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(got_vals, vec![4, 2]);

    for i in 0..out_len {
        free_string(unsafe { *out_keys.add(i) });
    }
    unsafe {
        drop(Vec::from_raw_parts(out_keys, out_len, out_len));
        drop(Vec::from_raw_parts(out_values, out_len, out_len));
    }
    free_string(kb);
    free_string(ka);
}

#[test]
fn map_param_scalar_return() {
    let mut err = ok_err();
    let ka = string_to_c_ptr("a");
    let kb = string_to_c_ptr("b");
    let keys: [*const c_char; 2] = [ka, kb];
    let values: [i32; 2] = [10, 32];
    let total = maps::weaveffi_maps_total(keys.as_ptr(), values.as_ptr(), 2, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(total, 42);
    free_string(ka);
    free_string(kb);
}

#[test]
fn builder_round_trip_and_required_field() {
    let mut err = ok_err();

    // Happy path: set every field, then build.
    let b = build::weaveffi_build_Widget_Builder_new();
    assert!(!b.is_null());
    let name = string_to_c_ptr("bolt");
    build::weaveffi_build_Widget_Builder_set_name(b, name);
    build::weaveffi_build_Widget_Builder_set_qty(b, 7);
    let note = string_to_c_ptr("aisle 4");
    build::weaveffi_build_Widget_Builder_set_note(b, note);
    free_string(name);
    free_string(note);

    let w = build::weaveffi_build_Widget_Builder_build(b, &mut err);
    assert_eq!(err.code, 0);
    assert!(!w.is_null());
    assert_eq!(build::weaveffi_build_Widget_get_qty(w), 7);
    let got_name = build::weaveffi_build_Widget_get_name(w);
    assert_eq!(c_ptr_to_string(got_name).unwrap(), "bolt");
    free_string(got_name);
    let got_note = build::weaveffi_build_Widget_get_note(w);
    assert_eq!(c_ptr_to_string(got_note).unwrap(), "aisle 4");
    free_string(got_note);
    build::weaveffi_build_Widget_destroy(w);
    build::weaveffi_build_Widget_Builder_destroy(b);

    // A required field (name) left unset surfaces an error at build time, and
    // an unset optional field (note) defaults to None.
    let b2 = build::weaveffi_build_Widget_Builder_new();
    build::weaveffi_build_Widget_Builder_set_qty(b2, 1);
    let w2 = build::weaveffi_build_Widget_Builder_build(b2, &mut err);
    assert!(w2.is_null());
    assert_ne!(err.code, 0);
    assert!(c_ptr_to_string(err.message).unwrap().contains("name"));
    abi::error_clear(&mut err);
    build::weaveffi_build_Widget_Builder_destroy(b2);
}

#[test]
fn rich_enum_tag_constructors_and_getters() {
    let mut err = ok_err();

    // Unit variant: tag 0, no fields.
    let empty = geom::weaveffi_geom_Shape_Empty_new(&mut err);
    assert_eq!(err.code, 0);
    assert_eq!(geom::weaveffi_geom_Shape_tag(empty), 0);

    // Struct variant carrying a scalar.
    let circle = geom::weaveffi_geom_Shape_Circle_new(2.5, &mut err);
    assert_eq!(geom::weaveffi_geom_Shape_tag(circle), 1);
    assert!((geom::weaveffi_geom_Shape_Circle_get_radius(circle) - 2.5).abs() < 1e-9);
    // A getter for a non-active variant yields the zero sentinel.
    assert_eq!(geom::weaveffi_geom_Shape_Labeled_get_count(circle), 0);

    // Struct variant carrying a string and a u8.
    let label = string_to_c_ptr("hex");
    let labeled = geom::weaveffi_geom_Shape_Labeled_new(label, 6, &mut err);
    free_string(label);
    assert_eq!(geom::weaveffi_geom_Shape_tag(labeled), 2);
    let got = geom::weaveffi_geom_Shape_Labeled_get_label(labeled);
    assert_eq!(c_ptr_to_string(got).unwrap(), "hex");
    free_string(got);
    assert_eq!(geom::weaveffi_geom_Shape_Labeled_get_count(labeled), 6);

    // A function taking the rich enum by reference.
    let d = geom::weaveffi_geom_describe(circle, &mut err);
    assert_eq!(c_ptr_to_string(d).unwrap(), "circle(2.5)");
    free_string(d);

    geom::weaveffi_geom_Shape_destroy(empty);
    geom::weaveffi_geom_Shape_destroy(circle);
    geom::weaveffi_geom_Shape_destroy(labeled);
}

#[test]
fn iterator_string_elements() {
    let mut err = ok_err();
    let iter = stream::weaveffi_stream_greetings(3, &mut err);
    assert_eq!(err.code, 0);
    assert!(!iter.is_null());

    let mut got = Vec::new();
    loop {
        let mut item: *const c_char = std::ptr::null();
        let has = stream::weaveffi_stream_GreetingsIterator_next(iter, &mut item, &mut err);
        assert_eq!(err.code, 0);
        if has == 0 {
            break;
        }
        got.push(c_ptr_to_string(item).unwrap());
        free_string(item);
    }
    stream::weaveffi_stream_GreetingsIterator_destroy(iter);
    assert_eq!(got, vec!["hi 0", "hi 1", "hi 2"]);
}

#[test]
fn iterator_scalar_elements() {
    let mut err = ok_err();
    let iter = stream::weaveffi_stream_squares(4, &mut err);
    let mut got = Vec::new();
    loop {
        let mut item: i32 = 0;
        if stream::weaveffi_stream_SquaresIterator_next(iter, &mut item, &mut err) == 0 {
            break;
        }
        got.push(item);
    }
    stream::weaveffi_stream_SquaresIterator_destroy(iter);
    assert_eq!(got, vec![0, 1, 4, 9]);
}

#[test]
fn listener_register_emit_unregister() {
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicI64, Ordering};

    static SUM: AtomicI64 = AtomicI64::new(0);
    extern "C" fn on_msg(text: *const c_char, weight: i32, ctx: *mut c_void) {
        assert!(!text.is_null());
        let counter = unsafe { &*(ctx as *const AtomicI64) };
        counter.fetch_add(weight as i64, Ordering::Relaxed);
    }

    SUM.store(0, Ordering::Relaxed);
    let id =
        bus::weaveffi_bus_register_subscribers(on_msg, &SUM as *const AtomicI64 as *mut c_void);
    assert!(id > 0);

    let mut err = ok_err();
    let text = string_to_c_ptr("hi");
    bus::weaveffi_bus_publish(text, 3, &mut err);
    bus::weaveffi_bus_publish(text, 5, &mut err);
    assert_eq!(err.code, 0);
    assert_eq!(SUM.load(Ordering::Relaxed), 8);

    bus::weaveffi_bus_unregister_subscribers(id);
    bus::weaveffi_bus_publish(text, 100, &mut err);
    assert_eq!(SUM.load(Ordering::Relaxed), 8);
    free_string(text);
}

#[test]
fn async_struct_result_completes_via_callback() {
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    type Msg = (bool, i64, String);
    extern "C" fn cb(ctx: *mut c_void, err: *mut weaveffi_error, result: *mut tasks::TaskResult) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<Msg>) };
        let had_err = !err.is_null() && unsafe { (*err).code } != 0;
        let payload = if result.is_null() {
            (had_err, 0, String::new())
        } else {
            let r = unsafe { &*result };
            (had_err, r.id, r.value.clone())
        };
        tx.send(payload).unwrap();
        if !result.is_null() {
            tasks::weaveffi_tasks_TaskResult_destroy(result);
        }
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_ptr = Box::into_raw(Box::new(tx));
    let name = string_to_c_ptr("alpha");
    tasks::weaveffi_tasks_run_task_async(name, cb, tx_ptr as *mut c_void);
    let (had_err, id, value) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    free_string(name);
    unsafe { drop(Box::from_raw(tx_ptr)) };

    assert!(!had_err);
    assert_eq!(id, 7);
    assert_eq!(value, "done: alpha");
}

#[test]
fn async_result_ok_and_err_paths() {
    use std::os::raw::c_void;
    use std::sync::mpsc;
    use std::time::Duration;

    type Msg = (bool, i32);
    extern "C" fn cb(ctx: *mut c_void, err: *mut weaveffi_error, result: i32) {
        let tx = unsafe { &*(ctx as *const mpsc::Sender<Msg>) };
        let had_err = !err.is_null() && unsafe { (*err).code } != 0;
        tx.send((had_err, result)).unwrap();
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_ptr = Box::into_raw(Box::new(tx));

    tasks::weaveffi_tasks_checked_add_async(2, 3, cb, tx_ptr as *mut c_void);
    let (had_err, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!had_err);
    assert_eq!(result, 5);

    tasks::weaveffi_tasks_checked_add_async(i32::MAX, 1, cb, tx_ptr as *mut c_void);
    let (had_err, result) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(had_err);
    assert_eq!(result, 0);

    unsafe { drop(Box::from_raw(tx_ptr)) };
}

/// Exercises nested-module codegen: the inner module's symbols must carry the
/// joined `outer_inner` path, and a nested function may reference a handle type
/// declared in its parent module via `super::` (the `kvstore` `stats` pattern).
#[weaveffi::module]
pub mod outer {
    /// A handle target type declared in the parent module.
    pub struct Session {
        /// The session id.
        pub id: i64,
    }

    /// Open a session, returning an opaque handle to it.
    #[weaveffi::export]
    pub fn open_session(id: i64) -> *mut Session {
        Box::into_raw(Box::new(Session { id }))
    }

    /// The nested sub-module: its symbols use the `outer_inner` prefix.
    #[weaveffi::module]
    pub mod inner {
        /// A by-value record produced by the nested module.
        #[weaveffi::record]
        #[derive(Clone)]
        pub struct Report {
            /// Ten times the session id.
            pub score: i64,
        }

        /// Summarize a parent-module `Session` handle into a nested `Report`.
        #[weaveffi::export]
        pub fn summarize(session: *const super::Session) -> Report {
            let id = unsafe { (*session).id };
            Report { score: id * 10 }
        }
    }
}

#[test]
fn nested_module_symbols_and_parent_type_reference() {
    let mut err = ok_err();
    let session = outer::weaveffi_outer_open_session(7, &mut err);
    assert_eq!(err.code, 0);
    assert!(!session.is_null());

    // The nested function is reachable at `outer::inner::*` and its symbol
    // carries the joined module path.
    let report = outer::inner::weaveffi_outer_inner_summarize(session, &mut err);
    assert_eq!(err.code, 0);
    assert!(!report.is_null());
    assert_eq!(
        outer::inner::weaveffi_outer_inner_Report_get_score(report),
        70
    );
    outer::inner::weaveffi_outer_inner_Report_destroy(report);

    unsafe { drop(Box::from_raw(session)) };
}

/// A producer module whose fallible function surfaces an IDL error domain's
/// named codes through [`weaveffi::ErrorReport`].
#[weaveffi::module]
pub mod vault {
    use weaveffi::ErrorReport;

    /// The vault's declared error domain: the codes consumers can match on.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum VaultError {
        /// entry not found
        NotFound = 2001,
        /// vault sealed
        Sealed = 2002,
    }

    /// The producer's internal failure type. It carries payloads (which the
    /// declared domain cannot), so it maps itself onto the domain's codes with
    /// a hand-written `ErrorReport` and dynamic messages. It deliberately does
    /// not implement `Display`, which would collide with the blanket impl.
    pub enum VaultFailure {
        /// No entry exists for the key.
        NotFound,
        /// The vault is sealed for the given reason.
        Sealed(String),
    }

    impl ErrorReport for VaultFailure {
        fn code(&self) -> i32 {
            match self {
                VaultFailure::NotFound => 2001,
                VaultFailure::Sealed(_) => 2002,
            }
        }
        fn message(&self) -> String {
            match self {
                VaultFailure::NotFound => "entry not found".to_string(),
                VaultFailure::Sealed(reason) => format!("vault sealed: {reason}"),
            }
        }
    }

    /// Fetch a doubled value, failing with a domain code for invalid keys.
    #[weaveffi::export]
    pub fn fetch(key: i64) -> Result<i64, VaultFailure> {
        match key {
            0 => Err(VaultFailure::NotFound),
            n if n < 0 => Err(VaultFailure::Sealed("negative key".to_string())),
            n => Ok(n * 2),
        }
    }
}

#[test]
fn fallible_with_domain_error_codes() {
    let mut err = ok_err();
    assert_eq!(vault::weaveffi_vault_fetch(21, &mut err), 42);
    assert_eq!(err.code, 0);

    // `Err` carries the producer-chosen code and message verbatim.
    let r = vault::weaveffi_vault_fetch(0, &mut err);
    assert_eq!(r, 0, "error path returns the zero sentinel");
    assert_eq!(err.code, 2001);
    assert_eq!(c_ptr_to_string(err.message).unwrap(), "entry not found");
    abi::error_clear(&mut err);

    let r = vault::weaveffi_vault_fetch(-1, &mut err);
    assert_eq!(r, 0);
    assert_eq!(err.code, 2002);
    assert_eq!(
        c_ptr_to_string(err.message).unwrap(),
        "vault sealed: negative key"
    );
    abi::error_clear(&mut err);
}

/// A producer module that exports a `#[deprecated]` function. The generated
/// thunk must still *call* the deprecated function, so it has to carry an
/// `#[allow(deprecated)]` of its own; otherwise the workspace's `-D warnings`
/// policy would reject the expansion. This module compiling at all is the
/// proof.
#[weaveffi::module]
pub mod legacy {
    /// The modern entry point.
    #[weaveffi::export]
    pub fn add_one(value: i64) -> i64 {
        value + 1
    }

    /// A retired entry point kept for one more release.
    #[deprecated(note = "use add_one")]
    #[weaveffi::export]
    pub fn bump(value: i64) -> i64 {
        value + 1
    }
}

/// A producer module built around an interface: an opaque object with
/// constructors, methods, statics, and a destructor.
#[weaveffi::module]
pub mod counters {
    use std::sync::atomic::{AtomicI64, Ordering};

    /// The counters error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CounterError {
        /// start value out of range
        OutOfRange = 1,
    }

    /// A monotonic counter, exported as an interface.
    #[weaveffi::interface]
    pub struct Counter {
        value: AtomicI64,
        step: i64,
    }

    impl Counter {
        /// Create a counter starting at `start`, stepping by 1.
        pub fn new(start: i64) -> Self {
            Self {
                value: AtomicI64::new(start),
                step: 1,
            }
        }

        /// Create a counter with a custom step, rejecting non-positive steps.
        pub fn with_step(start: i64, step: i64) -> Result<Counter, CounterError> {
            if step <= 0 {
                return Err(CounterError::OutOfRange);
            }
            Ok(Counter {
                value: AtomicI64::new(start),
                step,
            })
        }

        /// Advance the counter and return the new value.
        pub fn increment(&self) -> i64 {
            self.value.fetch_add(self.step, Ordering::Relaxed) + self.step
        }

        /// Read the current value without advancing.
        pub fn value(&self) -> i64 {
            self.value.load(Ordering::Relaxed)
        }

        /// Render the value with a prefix (string arg + string return).
        pub fn label(&self, prefix: &str) -> String {
            format!("{prefix}{}", self.value())
        }

        /// Clone the counter at its current value (interface return).
        pub fn snapshot(&self) -> Counter {
            Counter {
                value: AtomicI64::new(self.value()),
                step: self.step,
            }
        }

        /// Panic on purpose, proving panics surface as errors, not aborts.
        pub fn explode(&self) {
            panic!("counter exploded");
        }

        /// The default start value (a static under the interface namespace).
        pub fn default_start() -> i64 {
            0
        }

        // A private helper: not exported.
        #[allow(dead_code)]
        fn internal(&self) -> i64 {
            -1
        }
    }

    /// A free function taking the interface by reference.
    #[weaveffi::export]
    pub fn read_twice(counter: &Counter) -> i64 {
        counter.value() * 2
    }
}

#[test]
fn interface_constructor_methods_destroy() {
    let mut err = ok_err();

    let c = counters::weaveffi_counters_Counter_new(10, &mut err);
    assert_eq!(err.code, 0);
    assert!(!c.is_null());

    assert_eq!(
        counters::weaveffi_counters_Counter_increment(c, &mut err),
        11
    );
    assert_eq!(
        counters::weaveffi_counters_Counter_increment(c, &mut err),
        12
    );
    assert_eq!(counters::weaveffi_counters_Counter_value(c, &mut err), 12);
    assert_eq!(err.code, 0);

    let prefix = string_to_c_ptr("n=");
    let label = counters::weaveffi_counters_Counter_label(c, prefix, &mut err);
    assert_eq!(c_ptr_to_string(label).unwrap(), "n=12");
    free_string(label);
    free_string(prefix);

    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn interface_fallible_constructor() {
    let mut err = ok_err();

    let ok = counters::weaveffi_counters_Counter_with_step(0, 5, &mut err);
    assert_eq!(err.code, 0);
    assert!(!ok.is_null());
    assert_eq!(
        counters::weaveffi_counters_Counter_increment(ok, &mut err),
        5
    );
    counters::weaveffi_counters_Counter_destroy(ok);

    let bad = counters::weaveffi_counters_Counter_with_step(0, 0, &mut err);
    assert!(bad.is_null());
    assert_eq!(err.code, 1, "domain code from the #[weaveffi::error] enum");
    assert_eq!(
        c_ptr_to_string(err.message).unwrap(),
        "start value out of range"
    );
    abi::error_clear(&mut err);
}

#[test]
fn interface_returning_method_and_static() {
    let mut err = ok_err();
    assert_eq!(
        counters::weaveffi_counters_Counter_default_start(&mut err),
        0
    );

    let c = counters::weaveffi_counters_Counter_new(3, &mut err);
    let snap = counters::weaveffi_counters_Counter_snapshot(c, &mut err);
    assert!(!snap.is_null());
    counters::weaveffi_counters_Counter_increment(c, &mut err);
    assert_eq!(counters::weaveffi_counters_Counter_value(c, &mut err), 4);
    assert_eq!(
        counters::weaveffi_counters_Counter_value(snap, &mut err),
        3,
        "the snapshot is an independent object"
    );
    counters::weaveffi_counters_Counter_destroy(snap);
    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn interface_as_free_function_parameter() {
    let mut err = ok_err();
    let c = counters::weaveffi_counters_Counter_new(21, &mut err);
    assert_eq!(counters::weaveffi_counters_read_twice(c, &mut err), 42);
    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn interface_null_self_reports_error() {
    let mut err = ok_err();
    let r = counters::weaveffi_counters_Counter_value(std::ptr::null(), &mut err);
    assert_eq!(r, 0);
    assert_ne!(err.code, 0);
    abi::error_clear(&mut err);
}

#[test]
fn producer_panic_reports_panic_code() {
    let mut err = ok_err();
    let c = counters::weaveffi_counters_Counter_new(0, &mut err);

    counters::weaveffi_counters_Counter_explode(c, &mut err);
    assert_eq!(err.code, abi::PANIC_ERROR_CODE);
    assert!(c_ptr_to_string(err.message)
        .unwrap()
        .contains("counter exploded"));
    abi::error_clear(&mut err);

    // The object is still usable and the error slot resets on the next call.
    assert_eq!(counters::weaveffi_counters_Counter_value(c, &mut err), 0);
    assert_eq!(err.code, 0);
    counters::weaveffi_counters_Counter_destroy(c);
}

#[test]
fn deprecated_export_thunk_compiles_and_runs() {
    let mut err = ok_err();
    assert_eq!(legacy::weaveffi_legacy_add_one(41, &mut err), 42);
    assert_eq!(err.code, 0);

    // Calling the deprecated thunk would warn at this site, but the generated
    // thunk's own `#[allow(deprecated)]` keeps the macro expansion clean.
    #[allow(deprecated)]
    let bumped = legacy::weaveffi_legacy_bump(41, &mut err);
    assert_eq!(bumped, 42);
    assert_eq!(err.code, 0);
}
