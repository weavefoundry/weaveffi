/* libuv's Unix headers use POSIX types (pthread_rwlock_t, struct addrinfo)
 * that glibc hides under a strict -std=c11; ask for them explicitly. */
#if defined(__linux__) && !defined(_GNU_SOURCE)
#define _GNU_SOURCE
#endif
#ifndef NAPI_VERSION
#define NAPI_VERSION 8
#endif
#include <node_api.h>
#include <uv.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* The addon wraps deprecated functions too; the JavaScript API carries the
 * deprecation instead. */
#define {{MACRO}}_DEPRECATED(msg)
#include "{{HEADER}}"

/*
 * The N-API transport of the JavaScript bindings. Every C symbol of the
 * library is exported to `index.js` under its own name and called with the
 * raw convention the shared JavaScript layer documents: direct values as
 * numbers, bigints, and booleans (integers range-checked); optional scalars
 * as the value or null; typed arrays (numeric lists) as the matching
 * TypedArray; strings as strings; bytes and value buffers as Uint8Arrays;
 * objects, iterators, and cancel tokens as bigint handles (null when
 * absent); callback interfaces as adapter objects (null for an absent
 * optional one). A failure throws an instance of the runtime's `$Fault`
 * class.
 */

typedef {{PREFIX}}_error js_error;

/* Helpers an API may not need. */
#if defined(__GNUC__) || defined(__clang__)
#define JS_HELPER static inline __attribute__((unused))
#define JS_TLS __thread
#else
#define JS_HELPER static inline
#define JS_TLS __declspec(thread)
#endif

/* ---------------------------------------------------------------------------
 * JavaScript threads. One `js_thread` per thread that runs JavaScript and
 * loaded the addon (the main thread and each worker). It counts the
 * synchronous calls into the library in progress on that thread, which a
 * callback made on another thread consults while it waits for the
 * JavaScript thread (see js_cb_hop). Each environment and each callback
 * registration holds a reference.
 * ------------------------------------------------------------------------- */

typedef struct {
  uv_mutex_t mu;
  uv_cond_t cv;      /* broadcast when a call begins or ends, or a hop finishes */
  unsigned depth;    /* synchronous calls into the library in progress */
  uint64_t calls;    /* outermost synchronous calls begun so far */
  unsigned waiters;  /* producer threads waiting in js_cb_hop */
  unsigned refs;
} js_thread;

/* The state of the JavaScript thread this is, or NULL on any other thread. */
static JS_TLS js_thread* js_current = NULL;

static js_thread* js_thread_new(void) {
  js_thread* t = (js_thread*)calloc(1, sizeof *t);
  uv_mutex_init(&t->mu);
  uv_cond_init(&t->cv);
  t->refs = 1;
  return t;
}

static void js_thread_retain(js_thread* t) {
  uv_mutex_lock(&t->mu);
  t->refs++;
  uv_mutex_unlock(&t->mu);
}

static void js_thread_release(js_thread* t) {
  uv_mutex_lock(&t->mu);
  bool last = --t->refs == 0;
  uv_mutex_unlock(&t->mu);
  if (!last) return;
  uv_cond_destroy(&t->cv);
  uv_mutex_destroy(&t->mu);
  free(t);
}

/* Bracket every synchronous call into the library. */
JS_HELPER void js_sync_begin(void) {
  js_thread* t = js_current;
  if (t == NULL) return;
  uv_mutex_lock(&t->mu);
  if (t->depth++ == 0) t->calls++;
  if (t->waiters > 0) uv_cond_broadcast(&t->cv);
  uv_mutex_unlock(&t->mu);
}

JS_HELPER void js_sync_end(void) {
  js_thread* t = js_current;
  if (t == NULL) return;
  uv_mutex_lock(&t->mu);
  t->depth--;
  if (t->waiters > 0) uv_cond_broadcast(&t->cv);
  uv_mutex_unlock(&t->mu);
}

/* Per-environment state (one per main thread or worker): the `$Fault`
 * constructor `index.js` registers through `$setup`, and the thread. */
typedef struct {
  napi_ref fault;
  js_thread* thread;
} js_env;

static void js_env_free(napi_env env, void* data, void* hint) {
  (void)hint;
  js_env* e = (js_env*)data;
  if (e->fault != NULL) napi_delete_reference(env, e->fault);
  if (js_current == e->thread) js_current = NULL;
  js_thread_release(e->thread);
  free(e);
}

/* Set up the environment of the thread loading the addon. */
static void js_env_init(napi_env env) {
  js_env* e = (js_env*)calloc(1, sizeof *e);
  e->thread = js_thread_new();
  js_current = e->thread;
  napi_set_instance_data(env, e, js_env_free, NULL);
}

static js_env* js_env_of(napi_env env) {
  js_env* e = NULL;
  napi_get_instance_data(env, (void**)&e);
  return e;
}

JS_HELPER napi_value js_undefined(napi_env env) {
  napi_value v;
  napi_get_undefined(env, &v);
  return v;
}

static napi_value js_setup(napi_env env, napi_callback_info info) {
  size_t argc = 1;
  napi_value argv[1];
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL);
  js_env* e = js_env_of(env);
  if (e->fault != NULL) napi_delete_reference(env, e->fault);
  napi_create_reference(env, argv[0], 1, &e->fault);
  return js_undefined(env);
}

/* The `$Fault` constructor, or NULL before `$setup`. */
static napi_value js_fault_class(napi_env env) {
  js_env* e = js_env_of(env);
  napi_value ctor = NULL;
  if (e == NULL || e->fault == NULL || napi_get_reference_value(env, e->fault, &ctor) != napi_ok) {
    return NULL;
  }
  return ctor;
}

/* ---------------------------------------------------------------------------
 * Value conversions
 * ------------------------------------------------------------------------- */

JS_HELPER napi_value js_new_bytes(napi_env env, const uint8_t* ptr, size_t len) {
  napi_value ab, out;
  void* data = NULL;
  napi_create_arraybuffer(env, len, &data, &ab);
  if (len > 0) memcpy(data, ptr, len);
  napi_create_typedarray(env, napi_uint8_array, len, ab, 0, &out);
  return out;
}

JS_HELPER napi_value js_new_str(napi_env env, const uint8_t* ptr, size_t len) {
  napi_value out;
  napi_create_string_utf8(env, len == 0 ? "" : (const char*)ptr, len, &out);
  return out;
}

JS_HELPER napi_value js_new_i32(napi_env env, int32_t v) {
  napi_value out;
  napi_create_int32(env, v, &out);
  return out;
}

JS_HELPER napi_value js_new_u32(napi_env env, uint32_t v) {
  napi_value out;
  napi_create_uint32(env, v, &out);
  return out;
}

JS_HELPER napi_value js_new_i64(napi_env env, int64_t v) {
  napi_value out;
  napi_create_bigint_int64(env, v, &out);
  return out;
}

JS_HELPER napi_value js_new_u64(napi_env env, uint64_t v) {
  napi_value out;
  napi_create_bigint_uint64(env, v, &out);
  return out;
}

JS_HELPER napi_value js_new_f64(napi_env env, double v) {
  napi_value out;
  napi_create_double(env, v, &out);
  return out;
}

JS_HELPER napi_value js_new_bool(napi_env env, bool v) {
  napi_value out;
  napi_get_boolean(env, v, &out);
  return out;
}

JS_HELPER napi_value js_null(napi_env env) {
  napi_value out;
  napi_get_null(env, &out);
  return out;
}

/* An object, iterator, or cancel token handle: the pointer as a bigint, or
 * null for NULL. */
JS_HELPER napi_value js_new_handle(napi_env env, const void* p) {
  napi_value out;
  if (p == NULL) {
    napi_get_null(env, &out);
  } else {
    napi_create_bigint_uint64(env, (uint64_t)(uintptr_t)p, &out);
  }
  return out;
}

/* Returned strings and buffers: convert, then release the producer's copy. */
JS_HELPER napi_value js_take_str(napi_env env, const uint8_t* ptr, size_t len) {
  napi_value out = js_new_str(env, ptr, len);
  if (ptr != NULL) {{PREFIX}}_free_bytes((uint8_t*)ptr, len);
  return out;
}

JS_HELPER napi_value js_take_bytes(napi_env env, const uint8_t* ptr, size_t len) {
  napi_value out = js_new_bytes(env, ptr, len);
  if (ptr != NULL) {{PREFIX}}_free_bytes((uint8_t*)ptr, len);
  return out;
}

/* A typed array (numeric list) of `count` elements of `size` bytes: a
 * TypedArray of `type` holding a copy. A zero-length run is never read. */
JS_HELPER napi_value js_new_slice(napi_env env, napi_typedarray_type type, const void* ptr,
                                  size_t count, size_t size) {
  napi_value ab, out;
  void* data = NULL;
  napi_create_arraybuffer(env, count * size, &data, &ab);
  if (count > 0) memcpy(data, ptr, count * size);
  napi_create_typedarray(env, type, count, ab, 0, &out);
  return out;
}

/* A returned typed array: copy it, then release the producer's run. */
JS_HELPER napi_value js_take_slice(napi_env env, napi_typedarray_type type, const void* ptr,
                                   size_t count, size_t size) {
  napi_value out = js_new_slice(env, type, ptr, count, size);
  if (ptr != NULL) {{PREFIX}}_free_bytes((uint8_t*)ptr, count * size);
  return out;
}

/* A module's contract table as a BigUint64Array of `id, hash` pairs. */
JS_HELPER napi_value js_new_contract(napi_env env, const {{PREFIX}}_contract_entry* table,
                                     size_t len) {
  napi_value ab, out;
  void* data = NULL;
  napi_create_arraybuffer(env, len * 2 * sizeof(uint64_t), &data, &ab);
  uint64_t* words = (uint64_t*)data;
  for (size_t i = 0; i < len; i++) {
    words[2 * i] = table[i].id;
    words[2 * i + 1] = table[i].hash;
  }
  napi_create_typedarray(env, napi_biguint64_array, len * 2, ab, 0, &out);
  return out;
}

/* A `$Fault` for an error the library reported: its code, its message (a
 * length-delimited UTF-8 run, NULL for none), and its payload. */
JS_HELPER napi_value js_fault(napi_env env, int32_t code, const uint8_t* message,
                              size_t message_len, const uint8_t* payload, size_t payload_len) {
  napi_value argv[3], ctor, out = NULL;
  napi_create_int32(env, code, &argv[0]);
  argv[1] = js_new_str(env, message, message != NULL ? message_len : 0);
  if (payload != NULL) {
    argv[2] = js_new_bytes(env, payload, payload_len);
  } else {
    napi_get_null(env, &argv[2]);
  }
  ctor = js_fault_class(env);
  if (ctor == NULL || napi_new_instance(env, ctor, 3, argv, &out) != napi_ok) {
    napi_create_error(env, NULL, argv[1], &out);
    napi_set_named_property(env, out, "code", argv[0]);
  }
  return out;
}

/* Throw the error a call reported, then release it. */
JS_HELPER napi_value js_throw(napi_env env, js_error* err) {
  napi_throw(env, js_fault(env, err->code, err->message_ptr, err->message_len, err->payload_ptr,
                           err->payload_len));
  {{PREFIX}}_error_clear(err);
  return NULL;
}

JS_HELPER bool js_type_error(napi_env env, const char* expected) {
  bool pending = false;
  napi_is_exception_pending(env, &pending);
  if (!pending) napi_throw_type_error(env, NULL, expected);
  return false;
}

JS_HELPER bool js_range_error(napi_env env, const char* expected) {
  bool pending = false;
  napi_is_exception_pending(env, &pending);
  if (!pending) napi_throw_range_error(env, NULL, expected);
  return false;
}

/* An integer argument of at most 32 bits: a number that is an integer in
 * [lo, hi]. The range is checked on the double before any conversion, so
 * nothing wraps or truncates, on every platform. */
JS_HELPER bool js_arg_int(napi_env env, napi_value v, double lo, double hi, const char* expected,
                          double* out) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (t != napi_number) return js_type_error(env, "expected a number");
  double d = 0;
  napi_get_value_double(env, v, &d);
  if (!(d >= lo && d <= hi) || d != floor(d)) return js_range_error(env, expected);
  *out = d;
  return true;
}

JS_HELPER bool js_arg_i8(napi_env env, napi_value v, int8_t* out) {
  double d = 0;
  if (!js_arg_int(env, v, -128.0, 127.0, "expected an integer in [-128, 127]", &d)) return false;
  *out = (int8_t)d;
  return true;
}

JS_HELPER bool js_arg_i16(napi_env env, napi_value v, int16_t* out) {
  double d = 0;
  if (!js_arg_int(env, v, -32768.0, 32767.0, "expected an integer in [-32768, 32767]", &d)) {
    return false;
  }
  *out = (int16_t)d;
  return true;
}

JS_HELPER bool js_arg_i32(napi_env env, napi_value v, int32_t* out) {
  double d = 0;
  if (!js_arg_int(env, v, -2147483648.0, 2147483647.0,
                  "expected an integer in [-2147483648, 2147483647]", &d)) {
    return false;
  }
  *out = (int32_t)d;
  return true;
}

JS_HELPER bool js_arg_u8(napi_env env, napi_value v, uint8_t* out) {
  double d = 0;
  if (!js_arg_int(env, v, 0.0, 255.0, "expected an integer in [0, 255]", &d)) return false;
  *out = (uint8_t)d;
  return true;
}

JS_HELPER bool js_arg_u16(napi_env env, napi_value v, uint16_t* out) {
  double d = 0;
  if (!js_arg_int(env, v, 0.0, 65535.0, "expected an integer in [0, 65535]", &d)) return false;
  *out = (uint16_t)d;
  return true;
}

JS_HELPER bool js_arg_u32(napi_env env, napi_value v, uint32_t* out) {
  double d = 0;
  if (!js_arg_int(env, v, 0.0, 4294967295.0, "expected an integer in [0, 4294967295]", &d)) {
    return false;
  }
  *out = (uint32_t)d;
  return true;
}

/* 64-bit integers: a bigint that fits, or a number that is an integer in
 * range. The double is range-checked before it is converted (converting
 * an out-of-range double is undefined behavior in C); 2^63 and 2^64 are
 * exact doubles, so the bounds are exclusive. */
JS_HELPER bool js_arg_i64(napi_env env, napi_value v, int64_t* out) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (t == napi_bigint) {
    bool lossless = false;
    napi_get_value_bigint_int64(env, v, out, &lossless);
    return lossless || js_range_error(env, "expected a signed 64-bit integer");
  }
  if (t != napi_number) return js_type_error(env, "expected a bigint");
  double d = 0;
  napi_get_value_double(env, v, &d);
  if (!(d >= -9223372036854775808.0 && d < 9223372036854775808.0) || d != floor(d)) {
    return js_range_error(env, "expected a signed 64-bit integer");
  }
  *out = (int64_t)d;
  return true;
}

JS_HELPER bool js_arg_u64(napi_env env, napi_value v, uint64_t* out) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (t == napi_bigint) {
    bool lossless = false;
    napi_get_value_bigint_uint64(env, v, out, &lossless);
    return lossless || js_range_error(env, "expected an unsigned 64-bit integer");
  }
  if (t != napi_number) return js_type_error(env, "expected a bigint");
  double d = 0;
  napi_get_value_double(env, v, &d);
  if (!(d >= 0.0 && d < 18446744073709551616.0) || d != floor(d)) {
    return js_range_error(env, "expected an unsigned 64-bit integer");
  }
  *out = (uint64_t)d;
  return true;
}

JS_HELPER bool js_arg_f64(napi_env env, napi_value v, double* out) {
  return napi_get_value_double(env, v, out) == napi_ok || js_type_error(env, "expected a number");
}

/* f32: any number, rounded to the nearest float as Float32Array rounds it.
 * Converting a double outside the float range is undefined behavior in C,
 * so the overflow cases are explicit: at or past FLT_MAX plus half an ulp a
 * value rounds to an infinity, and below that to FLT_MAX. */
JS_HELPER bool js_arg_f32(napi_env env, napi_value v, float* out) {
  const double max = 3.4028234663852886e38;      /* FLT_MAX */
  const double overflow = 3.4028235677973366e38; /* FLT_MAX + half an ulp */
  double d = 0;
  if (!js_arg_f64(env, v, &d)) return false;
  if (d >= overflow) {
    *out = HUGE_VALF;
  } else if (d <= -overflow) {
    *out = -HUGE_VALF;
  } else if (d > max) {
    *out = (float)max;
  } else if (d < -max) {
    *out = (float)-max;
  } else {
    *out = (float)d;
  }
  return true;
}

JS_HELPER bool js_arg_bool(napi_env env, napi_value v, bool* out) {
  return napi_get_value_bool(env, v, out) == napi_ok || js_type_error(env, "expected a boolean");
}

/* Whether an optional argument is present (neither null nor undefined),
 * also stored in `*has`. */
JS_HELPER bool js_present(napi_env env, napi_value v, bool* has) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  *has = t != napi_null && t != napi_undefined;
  return *has;
}

/* A handle argument; null or undefined is NULL when `nullable`. */
JS_HELPER bool js_arg_handle(napi_env env, napi_value v, void** out, bool nullable) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (t == napi_null || t == napi_undefined) {
    *out = NULL;
    return nullable || js_type_error(env, "expected an object handle");
  }
  uint64_t raw = 0;
  bool lossless = false;
  if (t != napi_bigint || napi_get_value_bigint_uint64(env, v, &raw, &lossless) != napi_ok ||
      raw == 0) {
    return js_type_error(env, "expected an object handle");
  }
  *out = (void*)(uintptr_t)raw;
  return true;
}

/* A borrowed byte run: a Uint8Array (or Buffer). */
JS_HELPER bool js_arg_bytes(napi_env env, napi_value v, const uint8_t** ptr, size_t* len) {
  bool is_typed = false;
  napi_is_typedarray(env, v, &is_typed);
  if (is_typed) {
    napi_typedarray_type type;
    void* data = NULL;
    napi_get_typedarray_info(env, v, &type, len, &data, NULL, NULL);
    if (type == napi_uint8_array || type == napi_uint8_clamped_array) {
      *ptr = (const uint8_t*)data;
      return true;
    }
  }
  return js_type_error(env, "expected a Uint8Array");
}

/* A borrowed typed array (numeric list) of `type`: its data and element
 * count. */
JS_HELPER bool js_arg_slice(napi_env env, napi_value v, napi_typedarray_type type, const void** ptr,
                            size_t* count) {
  bool is_typed = false;
  napi_is_typedarray(env, v, &is_typed);
  if (is_typed) {
    napi_typedarray_type actual;
    void* data = NULL;
    napi_get_typedarray_info(env, v, &actual, count, &data, NULL, NULL);
    if (actual == type) {
      *ptr = data;
      return true;
    }
  }
  return js_type_error(env, "expected a typed array of the parameter's element type");
}

/* A string argument encoded as UTF-8: short strings live in the inline
 * buffer, longer ones on the heap until js_str_free. */
typedef struct {
  char* ptr;
  size_t len;
  char inline_buf[256];
} js_str;

#define JS_STR_INIT {NULL, 0, {0}}

JS_HELPER bool js_arg_str(napi_env env, napi_value v, js_str* s) {
  size_t len = 0;
  if (napi_get_value_string_utf8(env, v, NULL, 0, &len) != napi_ok) {
    return js_type_error(env, "expected a string");
  }
  s->ptr = len < sizeof s->inline_buf ? s->inline_buf : (char*)malloc(len + 1);
  napi_get_value_string_utf8(env, v, s->ptr, len + 1, &s->len);
  return true;
}

JS_HELPER void js_str_free(js_str* s) {
  if (s->ptr != NULL && s->ptr != s->inline_buf) free(s->ptr);
  s->ptr = NULL;
}

#define JS_STR_PTR(s) ((const uint8_t*)(s).ptr)

/* Read up to `n` arguments; missing ones are undefined. */
#define JS_ARGS(n)                                              \
  size_t argc = (n);                                            \
  napi_value argv[(n) + 1];                                     \
  napi_get_cb_info(env, info, &argc, argv, NULL, NULL)

/* Set `err` to `code` with a NUL-terminated message. */
JS_HELPER void js_error_set(js_error* err, int32_t code, const char* message) {
  {{PREFIX}}_error_set(err, code, (const uint8_t*)message, strlen(message));
}

/* ---------------------------------------------------------------------------
 * Async calls: the completion may run on any thread, so it only records the
 * result and queues the settlement on the JavaScript thread through a
 * thread-safe function (which also keeps the event loop alive meanwhile).
 * ------------------------------------------------------------------------- */

typedef enum {
  JS_R_VOID,
  JS_R_I32,
  JS_R_U32,
  JS_R_I64,
  JS_R_U64,
  JS_R_F64,
  JS_R_BOOL,
  JS_R_STR,
  JS_R_BYTES,
  JS_R_HANDLE,
  JS_R_SLICE
} js_kind;

/* One async call. The completion records its result here: the value (`v`,
 * plus `len` for a run), whether an optional scalar is present (`opt` and
 * `has`), and a typed array's type and element size. */
typedef struct {
  napi_deferred deferred;
  napi_threadsafe_function tsfn;
  js_kind kind;
  js_error* err;
  union {
    int64_t i;
    uint64_t u;
    double f;
    bool b;
    const void* p;
  } v;
  size_t len;
  bool opt;
  bool has;
  napi_typedarray_type slice_type;
  size_t slice_size;
} js_async;

JS_HELPER void js_async_settle(napi_env env, napi_value cb, void* context, void* data) {
  (void)cb;
  (void)context;
  js_async* a = (js_async*)data;
  bool failed = a->err != NULL && a->err->code != 0;
  if (env != NULL && failed) {
    napi_reject_deferred(env, a->deferred,
                         js_fault(env, a->err->code, a->err->message_ptr, a->err->message_len,
                                  a->err->payload_ptr, a->err->payload_len));
  } else if (env != NULL && a->opt && !a->has) {
    napi_resolve_deferred(env, a->deferred, js_null(env));
  } else if (env != NULL) {
    napi_value v = NULL;
    switch (a->kind) {
      case JS_R_VOID: v = js_undefined(env); break;
      case JS_R_I32: v = js_new_i32(env, (int32_t)a->v.i); break;
      case JS_R_U32: v = js_new_u32(env, (uint32_t)a->v.u); break;
      case JS_R_I64: v = js_new_i64(env, a->v.i); break;
      case JS_R_U64: v = js_new_u64(env, a->v.u); break;
      case JS_R_F64: v = js_new_f64(env, a->v.f); break;
      case JS_R_BOOL: v = js_new_bool(env, a->v.b); break;
      case JS_R_STR: v = js_take_str(env, (const uint8_t*)a->v.p, a->len); break;
      case JS_R_BYTES: v = js_take_bytes(env, (const uint8_t*)a->v.p, a->len); break;
      case JS_R_HANDLE: v = js_new_handle(env, a->v.p); break;
      case JS_R_SLICE:
        v = js_take_slice(env, a->slice_type, a->v.p, a->len, a->slice_size);
        break;
    }
    napi_resolve_deferred(env, a->deferred, v);
  } else if (!failed && (a->kind == JS_R_STR || a->kind == JS_R_BYTES) && a->v.p != NULL) {
    /* The environment is shutting down: just release the result. */
    {{PREFIX}}_free_bytes((uint8_t*)a->v.p, a->len);
  } else if (!failed && a->kind == JS_R_SLICE && a->v.p != NULL) {
    {{PREFIX}}_free_bytes((uint8_t*)a->v.p, a->len * a->slice_size);
  }
  {{PREFIX}}_error_free(a->err);
  napi_release_threadsafe_function(a->tsfn, napi_tsfn_release);
  free(a);
}

/* Create the promise and its settlement queue for one async call. */
JS_HELPER js_async* js_async_begin(napi_env env, js_kind kind, const char* name, napi_value* promise) {
  js_async* a = (js_async*)calloc(1, sizeof *a);
  a->kind = kind;
  napi_create_promise(env, &a->deferred, promise);
  napi_value resource;
  napi_create_string_utf8(env, name, NAPI_AUTO_LENGTH, &resource);
  napi_create_threadsafe_function(env, NULL, NULL, resource, 0, 1, NULL, NULL, NULL,
                                  js_async_settle, &a->tsfn);
  return a;
}

/* Called by each completion once it has recorded its result. */
JS_HELPER void js_async_done(js_async* a, js_error* err) {
  a->err = err;
  napi_call_threadsafe_function(a->tsfn, a, napi_tsfn_blocking);
}

/* ---------------------------------------------------------------------------
 * Callback interfaces. One registration per implementation passed to the
 * producer: a reference to the adapter object, the JavaScript thread it
 * belongs to, and a thread-safe function that runs calls made from any other
 * thread on that thread while the caller waits.
 *
 * Waiting is what makes a call from another thread safe, and also what could
 * deadlock it: if the JavaScript thread is itself inside a synchronous call
 * into the library that waits for the calling thread, neither can proceed.
 * A waiting call therefore watches the JavaScript thread. While that thread
 * stays inside one synchronous call for JS_CB_DEADLOCK_MS without having
 * started the callback, the wait is abandoned and the callback fails with
 * code -4 instead of hanging (a shorter synchronous call, or one that
 * returns to the event loop, just delays the callback).
 * ------------------------------------------------------------------------- */

#define JS_CB_DEADLOCK_MS 1000

typedef struct {
  napi_env env;
  napi_ref ref;
  napi_threadsafe_function tsfn;
  uv_thread_t js_thread;
  js_thread* thread;
} js_cb;

typedef enum { JS_REQ_QUEUED, JS_REQ_RUNNING, JS_REQ_DONE, JS_REQ_ABANDONED } js_req_state;

/* A call queued from another thread: `method` is the vtable index, or -1 to
 * release the registration; `frame` holds the method's slots. The request is
 * heap-allocated and holds its own reference to the thread state, so a
 * caller that abandons it can return (and the producer can then release the
 * registration) while it is still queued: the dispatcher frees it without
 * touching the frame or the registration. */
typedef struct {
  js_cb* cb;
  js_thread* thread;
  int method;
  void* frame;
  js_error* out_err;
  js_req_state state;
} js_cb_req;

JS_HELPER bool js_cb_on_js_thread(js_cb* cb) {
  uv_thread_t self = uv_thread_self();
  return uv_thread_equal(&self, &cb->js_thread) != 0;
}

JS_HELPER js_cb* js_cb_register(napi_env env, napi_value adapter, const char* name,
                                napi_threadsafe_function_call_js dispatch) {
  js_cb* cb = (js_cb*)calloc(1, sizeof *cb);
  cb->env = env;
  cb->js_thread = uv_thread_self();
  cb->thread = js_env_of(env)->thread;
  js_thread_retain(cb->thread);
  napi_create_reference(env, adapter, 1, &cb->ref);
  napi_value resource;
  napi_create_string_utf8(env, name, NAPI_AUTO_LENGTH, &resource);
  napi_create_threadsafe_function(env, NULL, NULL, resource, 0, 1, NULL, NULL, NULL, dispatch,
                                  &cb->tsfn);
  /* A live implementation must not keep the process alive by itself. */
  napi_unref_threadsafe_function(env, cb->tsfn);
  return cb;
}

JS_HELPER void js_cb_release(napi_env env, js_cb* cb) {
  if (env != NULL) napi_delete_reference(env, cb->ref);
  napi_release_threadsafe_function(cb->tsfn, napi_tsfn_release);
  js_thread_release(cb->thread);
  free(cb);
}

/* A callback-interface argument: the adapter object `index.js` built, or
 * null for an absent optional one. */
JS_HELPER bool js_arg_cb(napi_env env, napi_value v, const char* name,
                         napi_threadsafe_function_call_js dispatch, bool nullable, js_cb** out) {
  napi_valuetype t;
  napi_typeof(env, v, &t);
  if (nullable && (t == napi_null || t == napi_undefined)) {
    *out = NULL;
    return true;
  }
  if (t != napi_object) return js_type_error(env, "expected a callback interface implementation");
  *out = js_cb_register(env, v, name, dispatch);
  return true;
}

/* The vtable's `free`: the producer is done with the implementation. The
 * reference can only be deleted on its own thread. */
JS_HELPER void js_cb_free(void* ctx) {
  js_cb* cb = (js_cb*)ctx;
  if (cb == NULL) return;
  if (js_cb_on_js_thread(cb)) {
    js_cb_release(cb->env, cb);
    return;
  }
  js_cb_req* req = (js_cb_req*)calloc(1, sizeof *req);
  req->cb = cb;
  req->method = -1;
  if (napi_call_threadsafe_function(cb->tsfn, req, napi_tsfn_nonblocking) != napi_ok) free(req);
}

/* Run method `method` of `cb` on its JavaScript thread and wait for it,
 * failing it with code -4 rather than deadlocking (see above). `what` names
 * the method in that failure. */
JS_HELPER void js_cb_hop(js_cb* cb, int method, void* frame, js_error* out_err,
                         const char* what) {
  js_thread* t = cb->thread;
  js_cb_req* req = (js_cb_req*)calloc(1, sizeof *req);
  req->cb = cb;
  req->thread = t;
  js_thread_retain(t);
  req->method = method;
  req->frame = frame;
  req->out_err = out_err;
  req->state = JS_REQ_QUEUED;
  uv_mutex_lock(&t->mu);
  t->waiters++;
  uv_mutex_unlock(&t->mu);
  if (napi_call_threadsafe_function(cb->tsfn, req, napi_tsfn_nonblocking) != napi_ok) {
    uv_mutex_lock(&t->mu);
    t->waiters--;
    uv_mutex_unlock(&t->mu);
    free(req);
    js_thread_release(t);
    js_error_set(out_err, -4, "the callback implementation is no longer reachable");
    return;
  }
  const uint64_t limit = (uint64_t)JS_CB_DEADLOCK_MS * 1000000u;
  bool blocked = false;
  uint64_t call = 0, since = 0;
  uv_mutex_lock(&t->mu);
  while (req->state != JS_REQ_DONE) {
    if (req->state == JS_REQ_QUEUED && t->depth > 0) {
      uint64_t now = uv_hrtime();
      if (!blocked || call != t->calls) {
        blocked = true;
        call = t->calls;
        since = now;
      }
      if (now - since >= limit) {
        req->state = JS_REQ_ABANDONED; /* the dispatcher frees it */
        t->waiters--;
        uv_mutex_unlock(&t->mu);
        char msg[512];
        snprintf(msg, sizeof msg,
                 "%s was called on another thread while the JavaScript thread was blocked in a "
                 "synchronous call into the library for %d ms; the call likely waits for this "
                 "callback, so waiting longer would deadlock (call it asynchronously, or call "
                 "back on the calling thread)",
                 what, JS_CB_DEADLOCK_MS);
        js_error_set(out_err, -4, msg);
        return;
      }
      uv_cond_timedwait(&t->cv, &t->mu, limit - (now - since));
    } else {
      blocked = false;
      uv_cond_wait(&t->cv, &t->mu);
    }
  }
  t->waiters--;
  uv_mutex_unlock(&t->mu);
  free(req);
  js_thread_release(t);
}

/* The dispatcher's side of a hop. js_cb_take claims a queued request (false
 * when its caller gave up, in which case the request is freed);
 * js_cb_finish reports it done. */
JS_HELPER bool js_cb_take(js_cb_req* req) {
  js_thread* t = req->thread;
  uv_mutex_lock(&t->mu);
  bool abandoned = req->state == JS_REQ_ABANDONED;
  if (!abandoned) req->state = JS_REQ_RUNNING;
  uv_mutex_unlock(&t->mu);
  if (abandoned) {
    free(req);
    js_thread_release(t);
  }
  return !abandoned;
}

JS_HELPER void js_cb_finish(js_cb_req* req) {
  js_thread* t = req->thread;
  uv_mutex_lock(&t->mu);
  req->state = JS_REQ_DONE;
  uv_cond_broadcast(&t->cv);
  uv_mutex_unlock(&t->mu);
}

/* A JavaScript value's text as a heap-allocated C string (NULL on failure). */
JS_HELPER char* js_cstring(napi_env env, napi_value v) {
  napi_value str;
  size_t len = 0;
  if (napi_coerce_to_string(env, v, &str) != napi_ok ||
      napi_get_value_string_utf8(env, str, NULL, 0, &len) != napi_ok) {
    return NULL;
  }
  char* out = (char*)malloc(len + 1);
  napi_get_value_string_utf8(env, str, out, len + 1, NULL);
  return out;
}

/* Report the pending JavaScript exception (or `fallback`) through
 * `out_err`; nothing unwinds through the C frame. A `$Fault` (the adapter's
 * report of a method declared `throws`: a domain code with its payload, or
 * -1) reports its code, message, and payload; any other exception (from a
 * method that doesn't throw, or a return of the wrong type) is a callback
 * failure, code -4, with its message. */
JS_HELPER void js_cb_report(napi_env env, js_error* out_err, const char* fallback) {
  int32_t code = -4;
  char* msg = NULL;
  const uint8_t* payload = NULL;
  size_t payload_len = 0;
  bool pending = false;
  napi_is_exception_pending(env, &pending);
  if (pending) {
    napi_value exc, text;
    napi_get_and_clear_last_exception(env, &exc);
    napi_value fault = js_fault_class(env);
    bool is_fault = false;
    if (fault != NULL) napi_instanceof(env, exc, fault, &is_fault);
    napi_valuetype t;
    napi_typeof(env, exc, &t);
    text = exc;
    if (t == napi_object) napi_get_named_property(env, exc, "message", &text);
    msg = js_cstring(env, text);
    if (is_fault) {
      napi_value v;
      napi_get_named_property(env, exc, "code", &v);
      napi_get_value_int32(env, v, &code);
      napi_get_named_property(env, exc, "payload", &v);
      bool typed = false;
      napi_is_typedarray(env, v, &typed);
      if (typed) {
        napi_typedarray_type type;
        void* data = NULL;
        napi_get_typedarray_info(env, v, &type, &payload_len, &data, NULL, NULL);
        payload = (const uint8_t*)data;
      }
    }
    napi_is_exception_pending(env, &pending);
    if (pending) napi_get_and_clear_last_exception(env, &exc);
  }
  js_error_set(out_err, code, msg != NULL && msg[0] != 0 ? msg : fallback);
  if (payload != NULL) {{PREFIX}}_error_set_payload(out_err, payload, payload_len);
  free(msg);
}

/* Look up and call `method` on the adapter; false when it threw. */
JS_HELPER bool js_cb_call(napi_env env, js_cb* cb, const char* method, size_t argc,
                          const napi_value* argv, napi_value* result) {
  napi_value adapter, fn;
  napi_get_reference_value(env, cb->ref, &adapter);
  napi_get_named_property(env, adapter, method, &fn);
  return napi_call_function(env, adapter, fn, argc, argv, result) == napi_ok;
}

/* Hand `len` bytes to the producer through a callback's out slots, as a run
 * from {p}_alloc that the producer adopts. */
JS_HELPER void js_give(const void* data, size_t len, uint8_t** out_ptr, size_t* out_len) {
  uint8_t* run = {{PREFIX}}_alloc(len);
  if (len > 0) memcpy(run, data, len);
  *out_ptr = run;
  *out_len = len;
}

/* A callback's string return: false (with a pending exception) when the
 * value isn't a string. */
JS_HELPER bool js_ret_str(napi_env env, napi_value v, uint8_t** out_ptr, size_t* out_len) {
  js_str s = JS_STR_INIT;
  if (!js_arg_str(env, v, &s)) return false;
  js_give(s.ptr, s.len, out_ptr, out_len);
  js_str_free(&s);
  return true;
}

/* A callback's bytes or buffer return: a Uint8Array. */
JS_HELPER bool js_ret_bytes(napi_env env, napi_value v, uint8_t** out_ptr, size_t* out_len) {
  const uint8_t* ptr = NULL;
  size_t len = 0;
  if (!js_arg_bytes(env, v, &ptr, &len)) return false;
  js_give(ptr, len, out_ptr, out_len);
  return true;
}

/* A callback's typed-array return: a TypedArray of `type`, copied into a
 * {p}_alloc run of `count * size` bytes the producer adopts. `*run` and
 * `*count` are the run and its element count (NULL and 0 when empty). */
JS_HELPER bool js_ret_slice(napi_env env, napi_value v, napi_typedarray_type type, size_t size,
                            void** run, size_t* count) {
  const void* data = NULL;
  size_t n = 0;
  if (!js_arg_slice(env, v, type, &data, &n)) return false;
  uint8_t* out = {{PREFIX}}_alloc(n * size);
  if (n > 0) memcpy(out, data, n * size);
  *run = out;
  *count = n;
  return true;
}

/* ---------------------------------------------------------------------------
 * The runtime symbols every library exports.
 * ------------------------------------------------------------------------- */

static napi_value js_abi_version(napi_env env, napi_callback_info info) {
  (void)info;
  return js_new_u32(env, {{PREFIX}}_abi_version());
}

static napi_value js_debug_live(napi_env env, napi_callback_info info) {
  JS_ARGS(1);
  int32_t kind = 0;
  if (!js_arg_i32(env, argv[0], &kind)) return NULL;
  return js_new_u64(env, {{PREFIX}}_debug_live(kind));
}

static napi_value js_cancel_token_create(napi_env env, napi_callback_info info) {
  (void)info;
  return js_new_handle(env, {{PREFIX}}_cancel_token_create());
}

static napi_value js_cancel_token_cancel(napi_env env, napi_callback_info info) {
  JS_ARGS(1);
  void* token = NULL;
  if (!js_arg_handle(env, argv[0], &token, false)) return NULL;
  {{PREFIX}}_cancel_token_cancel(({{PREFIX}}_cancel_token*)token);
  return js_undefined(env);
}

static napi_value js_cancel_token_destroy(napi_env env, napi_callback_info info) {
  JS_ARGS(1);
  void* token = NULL;
  if (!js_arg_handle(env, argv[0], &token, false)) return NULL;
  {{PREFIX}}_cancel_token_destroy(({{PREFIX}}_cancel_token*)token);
  return js_undefined(env);
}

#define JS_RUNTIME_EXPORTS                                                                  \
  {"$setup", NULL, js_setup, NULL, NULL, NULL, napi_default, NULL},                         \
  {"{{PREFIX}}_abi_version", NULL, js_abi_version, NULL, NULL, NULL, napi_default, NULL},   \
  {"{{PREFIX}}_debug_live", NULL, js_debug_live, NULL, NULL, NULL, napi_default, NULL},     \
  {"{{PREFIX}}_cancel_token_create", NULL, js_cancel_token_create, NULL, NULL, NULL,        \
   napi_default, NULL},                                                                     \
  {"{{PREFIX}}_cancel_token_cancel", NULL, js_cancel_token_cancel, NULL, NULL, NULL,        \
   napi_default, NULL},                                                                     \
  {"{{PREFIX}}_cancel_token_destroy", NULL, js_cancel_token_destroy, NULL, NULL, NULL,      \
   napi_default, NULL}
