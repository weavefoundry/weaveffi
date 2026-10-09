/**
 * An error a call that declares errors reports.
 *
 * A positive code is a declared code of the module's error domain, thrown as
 * that domain's typed subclass. A negative code is a runtime failure: -1 a
 * generic producer error, -2 a producer panic, -3 a marshalling failure, and
 * -4 a callback-interface implementation that failed. A cancelled async call
 * (-5) throws Cancelled.
 */
class Error : public std::runtime_error {
    int32_t code_;

public:
    /** Builds an error carrying `code` and `message`. */
    Error(int32_t code, const std::string& message) : std::runtime_error(message), code_(code) {}

    /** The numeric error code. */
    int32_t code() const noexcept { return code_; }
};

/** Thrown by an async call that was cancelled through its CancelToken (code -5). */
class Cancelled : public Error {
public:
    /** Builds a cancellation error carrying the producer's message. */
    explicit Cancelled(const std::string& message) : Error(-5, message) {}
};

/**
 * Thrown by a call that declares no errors when it fails anyway: a producer
 * panic (-2), a marshalling failure (-3), a callback-interface failure the
 * producer let through (-4), or another runtime failure (-1). These are bugs,
 * so no signature declares them; `what()` names the code and the producer's
 * message.
 */
class InternalError : public std::runtime_error {
    int32_t code_;
    std::string message_;

public:
    /** Builds an internal error carrying `code` and the producer's `message`. */
    InternalError(int32_t code, const std::string& message)
        : std::runtime_error("{{LIBRARY}}: runtime error " + std::to_string(code) + ": " + message),
          code_(code),
          message_(message) {}

    /** The runtime error code (negative). */
    int32_t code() const noexcept { return code_; }

    /** The producer's message. */
    const std::string& message() const noexcept { return message_; }
};

/**
 * Thrown on first use when the linked {{LIBRARY}} library implements a
 * different C ABI revision than this header, or lacks or changed a
 * declaration this header was generated with.
 */
class LoadError : public std::runtime_error {
public:
    /** Builds a load error describing the mismatch. */
    explicit LoadError(const std::string& message) : std::runtime_error(message) {}
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

/**
 * A cancellation token for cancellable async functions.
 *
 * Create one, pass it to any number of cancellable calls, and call cancel()
 * from any thread. A cancelled call settles its future with Cancelled. Each
 * call takes its own reference to the native token, so the token can be
 * destroyed at any time; the destructor releases this wrapper's reference.
 */
class CancelToken {
    {{PREFIX}}_cancel_token* handle_;

    struct none_t {};
    explicit CancelToken(none_t) noexcept : handle_(nullptr) {}

public:
    /** Creates a new, uncancelled token. */
    CancelToken() : handle_({{PREFIX}}_cancel_token_create()) {
        if (handle_ == nullptr) throw std::bad_alloc();
    }

    /** Releases this wrapper's reference to the native token. */
    ~CancelToken() {
        if (handle_ != nullptr) {{PREFIX}}_cancel_token_destroy(handle_);
    }

    CancelToken(const CancelToken&) = delete;
    CancelToken& operator=(const CancelToken&) = delete;

    /** Transfers the native token; `other` becomes empty. */
    CancelToken(CancelToken&& other) noexcept : handle_(other.handle_) { other.handle_ = nullptr; }

    /** Releases the current token and takes over `other`'s. */
    CancelToken& operator=(CancelToken&& other) noexcept {
        if (this != &other) {
            if (handle_ != nullptr) {{PREFIX}}_cancel_token_destroy(handle_);
            handle_ = other.handle_;
            other.handle_ = nullptr;
        }
        return *this;
    }

    /** The empty token a cancellable call uses when none is passed. */
    static const CancelToken& none() noexcept {
        static const CancelToken token{none_t{}};
        return token;
    }

    /** Requests cancellation of every call this token was passed to. Idempotent and thread-safe. */
    void cancel() const noexcept {
        if (handle_ != nullptr) {{PREFIX}}_cancel_token_cancel(handle_);
    }

    /** Whether cancel() has been called. */
    bool is_cancelled() const noexcept {
        return handle_ != nullptr && {{PREFIX}}_cancel_token_is_cancelled(handle_);
    }

    /** The native token, borrowed (null for an empty token). */
    {{PREFIX}}_cancel_token* handle() const noexcept { return handle_; }
};

namespace detail {

/** The exception for a failed call that declares no errors: Cancelled for -5, else InternalError. */
inline std::exception_ptr make_internal_error(int32_t code, const std::string& message) {
    if (code == -5) return std::make_exception_ptr(Cancelled(message));
    return std::make_exception_ptr(InternalError(code, message));
}

/** The message of a nonzero error slot. */
inline std::string error_message(const {{PREFIX}}_error& err) {
    return std::string(err.message != nullptr ? err.message : "unknown error");
}

/** Throws InternalError if `err` carries a nonzero code (a call that declares no errors). */
inline void check({{PREFIX}}_error& err) {
    if (err.code == 0) return;
    std::exception_ptr ex = make_internal_error(err.code, error_message(err));
    {{PREFIX}}_error_clear(&err);
    std::rethrow_exception(ex);
}

/** Copies a producer-returned UTF-8 run into a string, then releases it. */
inline std::string take_string(const uint8_t* ptr, size_t len) {
    if (ptr == nullptr) return std::string();
    struct Release {
        const uint8_t* ptr;
        size_t len;
        ~Release() { {{PREFIX}}_free_bytes(const_cast<uint8_t*>(ptr), len); }
    } release{ptr, len};
    return std::string(reinterpret_cast<const char*>(ptr), len);
}

/** Copies a producer-returned byte run into a vector, then releases it. */
inline std::vector<uint8_t> take_bytes(const uint8_t* ptr, size_t len) {
    if (ptr == nullptr) return std::vector<uint8_t>();
    struct Release {
        const uint8_t* ptr;
        size_t len;
        ~Release() { {{PREFIX}}_free_bytes(const_cast<uint8_t*>(ptr), len); }
    } release{ptr, len};
    return std::vector<uint8_t>(ptr, ptr + len);
}

/** Views a borrowed UTF-8 run (null when empty) as a string_view. */
inline std::string_view borrow_string(const uint8_t* ptr, size_t len) noexcept {
    if (ptr == nullptr || len == 0) return std::string_view();
    return std::string_view(reinterpret_cast<const char*>(ptr), len);
}

/** Copies a borrowed byte run (null when empty) into a vector. */
inline std::vector<uint8_t> borrow_bytes(const uint8_t* ptr, size_t len) {
    if (ptr == nullptr || len == 0) return std::vector<uint8_t>();
    return std::vector<uint8_t>(ptr, ptr + len);
}

/** The data pointer of a string argument, as the C ABI expects it. */
inline const uint8_t* utf8(std::string_view s) noexcept {
    return reinterpret_cast<const uint8_t*>(s.data());
}

/**
 * Hands `len` bytes from `data` to the producer through a callback method's
 * out slots: copies them into a run from {{PREFIX}}_alloc, which the
 * producer adopts and frees.
 */
inline void hand_over(const void* data, size_t len, uint8_t** out_ptr, size_t* out_len) {
    uint8_t* run = nullptr;
    if (len > 0) {
        run = {{PREFIX}}_alloc(len);
        if (run == nullptr) throw std::bad_alloc();
        std::memcpy(run, data, len);
    }
    *out_ptr = run;
    *out_len = len;
}

/** One declaration this header was generated with: its contract id and hash, and its dotted path. */
struct ContractEntry {
    /** FNV-1a 64 of the declaration's dotted path. */
    uint64_t id;
    /** FNV-1a 64 of the declaration's canonical signature. */
    uint64_t hash;
    /** The declaration's dotted path (`kv.Store.put`). */
    const char* path;
};

/**
 * Why the contract table `contract` returns doesn't satisfy `expected`,
 * naming the first declaration it lacks or declares differently, or an empty
 * string when it has them all. Entries the producer has and `expected`
 * doesn't are fine.
 */
template <size_t N>
std::string contract_mismatch(const {{PREFIX}}_contract_entry* (*contract)(size_t*),
                              const ContractEntry (&expected)[N]) {
    size_t len = 0;
    const {{PREFIX}}_contract_entry* table = contract(&len);
    if (table == nullptr) len = 0;
    for (const ContractEntry& want : expected) {
        size_t lo = 0;
        size_t hi = len;
        while (lo < hi) {
            size_t mid = lo + (hi - lo) / 2;
            if (table[mid].id < want.id) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if (lo == len || table[lo].id != want.id) {
            return std::string(want.path) + " is missing from the library";
        }
        if (table[lo].hash != want.hash) {
            return std::string(want.path) + " changed since these bindings were generated";
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

