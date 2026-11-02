/**
 * The base of every exception this library throws.
 *
 * `code()` is the failure's error code. A positive code belongs to an error
 * domain and is thrown as that domain's class (or the class of the code).
 * A negative code is a runtime failure: -1 an untyped error (a call declared
 * `throws: any`), -2 a producer panic, -3 a marshalling failure, -4 a
 * callback-interface implementation that failed, and -5 a cancelled async
 * call (thrown as Cancelled). Code 0 is a failure these bindings detected
 * without calling the producer (LoadError).
 */
class Error : public std::runtime_error {
public:
    /** Builds an error carrying `code` and `message`. */
    Error(int32_t code, const std::string& message) : std::runtime_error(message), code_(code) {}

    /** The numeric error code. */
    int32_t code() const noexcept { return code_; }

private:
    int32_t code_;
};

/** Thrown by an async call that was cancelled through its CancelToken (code -5). */
class Cancelled : public Error {
public:
    /** Builds a cancellation error carrying the producer's message. */
    explicit Cancelled(const std::string& message) : Error(-5, message) {}
};

/**
 * Thrown when a call that declares no errors fails anyway: a producer panic
 * (-2), a marshalling failure (-3), a callback-interface failure the producer
 * let through (-4), or another runtime failure. These are bugs, so no
 * signature declares them. A value buffer these bindings can't decode is
 * also an InternalError with code -3.
 */
class InternalError : public Error {
public:
    /** Builds an internal error carrying the runtime `code` and the producer's `message`. */
    InternalError(int32_t code, const std::string& message) : Error(code, message) {}
};

/**
 * Thrown on first use when the linked {{LIBRARY}} library implements a
 * different C ABI revision than this header, or lacks or changed a
 * declaration this header was generated with. Its code is 0.
 */
class LoadError : public Error {
public:
    /** Builds a load error describing the mismatch. */
    explicit LoadError(const std::string& message) : Error(0, message) {}
};

/**
 * Tag selecting the constructor that adopts a raw C pointer (one strong
 * reference the wrapper then owns), in the style of std::adopt_lock.
 */
struct adopt_t {
    explicit adopt_t() = default;
};

/** The adopt_t tag value: `Store s(adopt, raw)`. */
inline constexpr adopt_t adopt{};

namespace detail {

/** The C error slot every fallible call fills. */
using error = {{PREFIX}}_error;

/** The message of a failed call's error slot (empty when the producer sent none). */
inline std::string message(const error& err) {
    if (err.message_ptr == nullptr || err.message_len == 0) return std::string();
    return std::string(reinterpret_cast<const char*>(err.message_ptr), err.message_len);
}

/** Fills a callback's error slot with `code` and a copy of `message`. */
inline void set_error(error* out_err, int32_t code, std::string_view message) noexcept {
    {{PREFIX}}_error_set(out_err, code, reinterpret_cast<const uint8_t*>(message.data()), message.size());
}

/**
 * Reports the exception being handled through a callback's error slot as
 * `code` with its `what()`. Call it only from inside a catch handler.
 */
inline void report_current(error* out_err, int32_t code) noexcept {
    try {
        throw;
    } catch (const std::exception& e) {
        set_error(out_err, code, e.what());
    } catch (...) {
        set_error(out_err, code, "a callback implementation threw an exception not derived from std::exception");
    }
}

/**
 * How a call that throws `E` turns a failed error slot into an exception
 * (`raise`), and how a callback method that may throw `E` reports the
 * exception being handled to the producer (`report`). Specialized for
 * InternalError (calls that declare no errors), Error (`throws: any`), and
 * each error domain.
 */
template <typename E>
struct Errors;

/** Throws Cancelled for -5 and `E` for every other code. */
template <typename E>
[[noreturn]] void raise_runtime(const error& err) {
    if (err.code == -5) throw Cancelled(message(err));
    throw E(err.code, message(err));
}

/** A call that declares no errors: any failure is an InternalError. A callback method reports -4. */
template <>
struct Errors<InternalError> {
    [[noreturn]] static void raise(const error& err) { raise_runtime<InternalError>(err); }
    static void report(error* out_err) noexcept { report_current(out_err, -4); }
};

/** A call declared `throws: any`: any failure is an Error. A callback method reports -1. */
template <>
struct Errors<Error> {
    [[noreturn]] static void raise(const error& err) { raise_runtime<Error>(err); }
    static void report(error* out_err) noexcept { report_current(out_err, -1); }
};

/** Releases an error slot's message and payload on scope exit. */
struct ErrorClear {
    error* err;
    ~ErrorClear() { {{PREFIX}}_error_clear(err); }
};

/** Throws the `E` exception for a failed call's error slot, which it then clears. */
template <typename E>
void check(error& err) {
    if (err.code == 0) return;
    ErrorClear clear{&err};
    Errors<E>::raise(err);
}

/** Releases an async completion's heap-boxed error. */
struct ErrorFree {
    void operator()(error* err) const noexcept { {{PREFIX}}_error_free(err); }
};

/**
 * Settles the `std::promise<T>` an async call passed as `context`, exactly
 * once: with the `E` exception for a failure, else with `value()` (which
 * takes ownership of the result). Releases the boxed error. Runs on
 * a producer thread inside a C frame, so nothing unwinds out of it.
 */
template <typename E, typename T, typename F>
void settle(void* context, error* err, F&& value) noexcept {
    std::unique_ptr<std::promise<T>> promise(static_cast<std::promise<T>*>(context));
    std::unique_ptr<error, ErrorFree> owned(err);
    try {
        if (err != nullptr && err->code != 0) Errors<E>::raise(*err);
        if constexpr (std::is_void_v<T>) {
            value();
            promise->set_value();
        } else {
            promise->set_value(value());
        }
    } catch (...) {
        promise->set_exception(std::current_exception());
    }
}

/**
 * Runs a callback method's `body` for a trampoline, reporting any exception
 * through `out_err` as a method that throws `E` does. Returns the body's
 * value, or a zero value after a failure.
 */
template <typename E, typename F>
auto callback(error* out_err, F&& body) noexcept -> decltype(body()) {
    try {
        return body();
    } catch (...) {
        Errors<E>::report(out_err);
    }
    if constexpr (!std::is_void_v<decltype(body())>) return decltype(body()){};
}

/** The C value of a string, bytes, typed array, or value buffer: a run of bytes. */
using run = {{PREFIX}}_run;

/** Releases a run the producer returned (a string, bytes, typed array, or value buffer) on scope exit. */
struct OwnedRun {
    run value;
    ~OwnedRun() { {{PREFIX}}_run_free(value); }
};

/** A borrowed run over a string argument's UTF-8 bytes (no terminator, so interior NULs survive). */
inline run string_run(std::string_view s) noexcept {
    return run{reinterpret_cast<const uint8_t*>(s.data()), s.size()};
}

/** A borrowed run over a bytes argument. */
inline run bytes_run(const std::vector<uint8_t>& v) noexcept { return run{v.data(), v.size()}; }

/** A borrowed run over a typed-array argument's storage: `len` counts bytes. */
template <typename T>
run slice_run(const std::vector<T>& v) noexcept {
    return run{reinterpret_cast<const uint8_t*>(v.data()), v.size() * sizeof(T)};
}

/** Views a borrowed UTF-8 run as a string_view, valid while the run is. */
inline std::string_view borrow_string(run r) noexcept {
    if (r.len == 0) return std::string_view();
    return std::string_view(reinterpret_cast<const char*>(r.ptr), r.len);
}

/** Copies a borrowed byte run into a vector. */
inline std::vector<uint8_t> borrow_bytes(run r) {
    if (r.len == 0) return std::vector<uint8_t>();
    return std::vector<uint8_t>(r.ptr, r.ptr + r.len);
}

/** Copies a borrowed typed array into a vector, failing with -3 when its length isn't a whole number of elements. */
template <typename T>
std::vector<T> borrow_slice(run r) {
    if (r.len % sizeof(T) != 0) throw InternalError(-3, "typed array length is not a multiple of its element size");
    std::vector<T> v(r.len / sizeof(T));
    if (r.len != 0) std::memcpy(v.data(), r.ptr, r.len);
    return v;
}

/** Copies a producer-returned UTF-8 run into a string, then releases it. */
inline std::string take_string(run r) {
    OwnedRun owned{r};
    return std::string(borrow_string(r));
}

/** Copies a producer-returned byte run into a vector, then releases it. */
inline std::vector<uint8_t> take_bytes(run r) {
    OwnedRun owned{r};
    return borrow_bytes(r);
}

/** Copies a producer-returned typed array into a vector, then releases it. */
template <typename T>
std::vector<T> take_slice(run r) {
    OwnedRun owned{r};
    return borrow_slice<T>(r);
}

/**
 * Hands `len` bytes from `data` to the producer as a callback method's
 * return: copies them into a run from {{PREFIX}}_alloc, which the producer
 * adopts and frees.
 */
inline run give_run(const void* data, size_t len) {
    if (len == 0) return run{nullptr, 0};
    uint8_t* ptr = {{PREFIX}}_alloc(len);
    if (ptr == nullptr) throw std::bad_alloc();
    std::memcpy(ptr, data, len);
    return run{ptr, len};
}

/** Hands a callback's string return to the producer. */
inline run give_string(std::string_view value) { return give_run(value.data(), value.size()); }

/** Hands a callback's bytes return to the producer. */
inline run give_bytes(const std::vector<uint8_t>& value) { return give_run(value.data(), value.size()); }

/** Hands a callback's typed-array return to the producer. */
template <typename T>
run give_slice(const std::vector<T>& value) {
    return give_run(value.data(), value.size() * sizeof(T));
}

/** The C value of an optional scalar or C-style enum (`O` is its `{{PREFIX}}_opt_*` struct). */
template <typename O, typename T>
O to_opt(const std::optional<T>& value) noexcept {
    O out{};
    if (value.has_value()) {
        out.has = true;
        out.value = static_cast<decltype(out.value)>(*value);
    }
    return out;
}

/** An optional scalar or C-style enum from its `{{PREFIX}}_opt_*` struct. */
template <typename T, typename O>
std::optional<T> from_opt(const O& value) noexcept {
    if (!value.has) return std::nullopt;
    return std::optional<T>(static_cast<T>(value.value));
}

/** A wrapper adopting a returned object reference, or none for null. */
template <typename T>
std::optional<T> adopt_optional(typename T::raw_type* raw) noexcept {
    if (raw == nullptr) return std::nullopt;
    return std::optional<T>(std::in_place, adopt, raw);
}

/** The borrowed C pointer of an optional object argument (null for none). */
template <typename T>
const typename T::raw_type* handle_of(const std::optional<T>& object) noexcept {
    return object.has_value() ? object->handle() : nullptr;
}

/** A new strong reference to an optional object a callback returns (null for none). */
template <typename T>
typename T::raw_type* clone_of(const std::optional<T>& object) noexcept {
    return object.has_value() ? object->clone_handle() : nullptr;
}

/** The implementation a callback interface's `ctx` boxes. */
template <typename I>
I& implementation(void* ctx) noexcept {
    return **static_cast<std::shared_ptr<I>*>(ctx);
}

/** Deletes a callback interface's `ctx` box once the producer releases it. */
template <typename I>
void release(void* ctx) noexcept {
    delete static_cast<std::shared_ptr<I>*>(ctx);
}

/**
 * Boxes an implementation as a callback interface's `ctx` (null for an
 * empty pointer). The caller releases the box into the call, after which
 * the producer owns it and deletes it through the vtable's `free`.
 */
template <typename I>
std::unique_ptr<std::shared_ptr<I>> lend(std::shared_ptr<I> impl) {
    if (!impl) return nullptr;
    return std::make_unique<std::shared_ptr<I>>(std::move(impl));
}

/** Boxes a required implementation, failing with -3 (naming `param`) when it's empty. */
template <typename I>
std::unique_ptr<std::shared_ptr<I>> lend_required(std::shared_ptr<I> impl, const char* param) {
    if (!impl) throw Error(-3, std::string(param) + ": null callback interface");
    return lend(std::move(impl));
}

/**
 * The trampolines and static vtable that adapt implementations of the
 * callback interface `I` to its C vtable; specialized per callback
 * interface.
 */
template <typename I>
struct Callbacks;

/**
 * Owns one strong reference to a reference-counted producer object, the
 * Rule-of-Zero core of every interface wrapper: copying takes a new
 * reference through `Traits::clone`, moving transfers it (leaving null), and
 * destruction releases it through `Traits::destroy`.
 */
template <typename Traits>
class Handle {
public:
    /** The C type of the object. */
    using raw_type = typename Traits::raw_type;

    /** An empty handle. */
    Handle() noexcept = default;

    /** Adopts one strong reference (or null). */
    explicit Handle(raw_type* raw) noexcept : raw_(raw) {}

    /** Takes a new strong reference to `other`'s object. */
    Handle(const Handle& other) noexcept : raw_(other.clone()) {}

    /** Transfers `other`'s reference; `other` becomes empty. */
    Handle(Handle&& other) noexcept : raw_(std::exchange(other.raw_, nullptr)) {}

    /** Copy-and-swap assignment from a copy or a moved-from handle. */
    Handle& operator=(Handle other) noexcept {
        std::swap(raw_, other.raw_);
        return *this;
    }

    /** Releases the reference. */
    ~Handle() {
        if (raw_ != nullptr) Traits::destroy(raw_);
    }

    /** The object, borrowed (null when empty). */
    raw_type* get() const noexcept { return raw_; }

    /** A new strong reference to the object, which the caller owns (null when empty). */
    raw_type* clone() const noexcept { return raw_ != nullptr ? Traits::clone(raw_) : nullptr; }

    /** Releases the current reference and adopts `raw`. */
    void reset(raw_type* raw) noexcept { *this = Handle(raw); }

private:
    raw_type* raw_ = nullptr;
};

} // namespace detail

/**
 * A cancellation token for cancellable async functions.
 *
 * Create one, pass it to any number of cancellable calls, and call cancel()
 * from any thread. A cancelled call settles its future with Cancelled. Each
 * call takes its own reference to the native token, so the token can be
 * destroyed at any time. Move-only.
 */
class CancelToken {
public:
    /** Creates a new, uncancelled token. */
    CancelToken() : handle_({{PREFIX}}_cancel_token_create()) {
        if (!handle_) throw std::bad_alloc();
    }

    /** The empty token a cancellable call uses when none is passed; it never cancels. */
    static const CancelToken& none() noexcept {
        static const CancelToken token{nullptr};
        return token;
    }

    /** Requests cancellation of every call this token was passed to. Idempotent and thread-safe. */
    void cancel() const noexcept {
        if (handle_) {{PREFIX}}_cancel_token_cancel(handle_.get());
    }

    /** Whether cancel() has been called. */
    bool is_cancelled() const noexcept { return handle_ && {{PREFIX}}_cancel_token_is_cancelled(handle_.get()); }

    /** The native token, borrowed (null for an empty token). */
    {{PREFIX}}_cancel_token* handle() const noexcept { return handle_.get(); }

private:
    struct Destroy {
        void operator()({{PREFIX}}_cancel_token* token) const noexcept { {{PREFIX}}_cancel_token_destroy(token); }
    };

    explicit CancelToken(std::nullptr_t) noexcept {}

    std::unique_ptr<{{PREFIX}}_cancel_token, Destroy> handle_;
};

/**
 * A lazy, single-pass, move-only range over the elements a producer
 * iterator yields: what every `iter<T>` callable returns.
 *
 * Each step makes one producer call, so results stream in constant memory:
 *
 *     for (const std::string& key : store.keys(std::nullopt)) { ... }
 *
 * The range releases the producer iterator exactly once: when it's
 * exhausted, when a step fails (rethrowing the call's exception), on
 * close(), or from the destructor when iteration stops early. Moving a range
 * invalidates its iterators.
 */
template <typename T>
class Range {
public:
    /** The element type. */
    using value_type = T;

    /** Pulls one element into `item`; returns false once exhausted, throws on a producer error. */
    using Next = bool (*)(void* iter, std::optional<T>& item);

    /** Releases the producer iterator. */
    using Destroy = void (*)(void* iter);

    /** Adopts a producer iterator, driven by `next` and released by `destroy`. */
    Range(adopt_t, void* iter, Next next, Destroy destroy) noexcept : iter_(iter, destroy), next_(next) {}

    /** Pulls the next element, or `std::nullopt` once the range is exhausted. */
    std::optional<T> next() {
        std::optional<T> item;
        if (!iter_) return item;
        bool more = false;
        try {
            more = next_(iter_.get(), item);
        } catch (...) {
            iter_.reset();
            throw;
        }
        if (!more) iter_.reset();
        return item;
    }

    /** Releases the producer iterator now; the range is then exhausted. */
    void close() noexcept { iter_.reset(); }

    /** Marks the end of the range. */
    struct sentinel {};

    /** A single-pass input iterator; each increment pulls one element. */
    class iterator {
    public:
        using iterator_category = std::input_iterator_tag;
        using value_type = T;
        using difference_type = std::ptrdiff_t;
        using pointer = T*;
        using reference = T&;

        iterator() noexcept = default;

        reference operator*() const { return *range_->current_; }
        pointer operator->() const { return &*range_->current_; }

        iterator& operator++() {
            range_->current_ = range_->next();
            return *this;
        }

        void operator++(int) { ++*this; }

        friend bool operator==(const iterator& it, sentinel) noexcept { return it.done(); }
        friend bool operator==(sentinel end, const iterator& it) noexcept { return it == end; }
        friend bool operator!=(const iterator& it, sentinel end) noexcept { return !(it == end); }
        friend bool operator!=(sentinel end, const iterator& it) noexcept { return !(it == end); }

    private:
        friend class Range;
        explicit iterator(Range* range) noexcept : range_(range) {}
        bool done() const noexcept { return !range_->current_.has_value(); }
        Range* range_ = nullptr;
    };

    /** Begins iteration by pulling the first element. */
    iterator begin() {
        current_ = next();
        return iterator(this);
    }

    /** The end sentinel. */
    sentinel end() const noexcept { return sentinel{}; }

private:
    std::unique_ptr<void, Destroy> iter_;
    Next next_;
    std::optional<T> current_;
};

namespace detail {

/**
 * One declaration this header was generated with: the library's contract
 * function for it, the hash of its canonical signature, and its dotted path.
 */
struct ContractRow {
    /** The library's `{{PREFIX}}_contract_*` function, which returns the declaration's hash. */
    uint64_t (*hash)();
    /** FNV-1a 64 of the declaration's canonical signature, as generated. */
    uint64_t expected;
    /** The declaration's dotted path (`Store.put`). */
    const char* path;
};

/**
 * Why the linked library doesn't match `rows`, naming the first declaration
 * it declares differently, or an empty string when it matches them all. A
 * declaration the library lacks fails to link instead, and declarations it
 * has beyond `rows` are fine.
 */
template <size_t N>
std::string contract_mismatch(const ContractRow (&rows)[N]) {
    for (const ContractRow& row : rows) {
        if (row.hash() != row.expected) {
            return std::string(row.path) + " changed since these bindings were generated";
        }
    }
    return std::string();
}

{{CONTRACTS}}} // namespace detail

/**
 * Verifies that the linked {{LIBRARY}} library implements C ABI revision
 * {{ABI}} and every declaration this header was generated with, throwing
 * LoadError naming the first mismatch. The check runs once; every free
 * function, constructor, and static member calls this before its first
 * native call, so calling it at startup is optional.
 */
inline void check_library() {
    static const std::string failure = []() -> std::string {
        uint32_t abi = {{PREFIX}}_abi_version();
        if (abi != {{MACRO}}_ABI_VERSION) {
            return "{{LIBRARY}}: C ABI revision mismatch (header " + std::to_string({{MACRO}}_ABI_VERSION) +
                   ", library " + std::to_string(abi) + ")";
        }
{{CHECKS}}        return std::string();
    }();
    if (!failure.empty()) throw LoadError(failure);
}
