/**
 * Base exception for every error reported through the C ABI.
 *
 * Positive codes are the declared error codes (see the typed domain
 * exceptions); negative codes are runtime traps: -1 a generic producer
 * error, -2 a producer panic, -3 a marshalling failure, -4 a
 * callback-interface implementation that threw, and -5 a cancelled call
 * (thrown as Cancelled).
 */
class Error : public std::runtime_error {
    int32_t code_;

public:
    /** Builds an error carrying `code` and `message`. */
    Error(int32_t code, const std::string& message) : std::runtime_error(message), code_(code) {}

    /** The numeric error code. */
    int32_t code() const noexcept { return code_; }
};

/** Thrown by an async call whose CancelToken was cancelled (code -5). */
class Cancelled : public Error {
public:
    /** Builds a cancellation error carrying the producer's message. */
    explicit Cancelled(const std::string& message) : Error(-5, message) {}
};

/**
 * Thrown on first use when the linked {{LIBRARY}} library was built for a
 * different C ABI revision or a different API contract than this header.
 */
class LoadError : public Error {
public:
    /** Builds a load error describing the mismatch. */
    explicit LoadError(const std::string& message) : Error(-1, message) {}
};

/**
 * Tag selecting the constructor that adopts a raw C pointer (one strong
 * reference the wrapper then owns), in the style of std::adopt_lock.
 */
struct adopt_t {
    explicit adopt_t() = default;
};

/** The adopt_t tag value: `Gadget g(adopt, raw)`. */
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

/** The exception for a runtime (non-domain) error code. */
inline std::exception_ptr make_error(int32_t code, const std::string& message) {
    if (code == -5) return std::make_exception_ptr(Cancelled(message));
    return std::make_exception_ptr(Error(code, message));
}

/** The message of a nonzero error slot. */
inline std::string error_message(const {{PREFIX}}_error& err) {
    return std::string(err.message != nullptr ? err.message : "unknown error");
}

/** Throws the generic Error (or Cancelled) if `err` carries a nonzero code. */
inline void check({{PREFIX}}_error& err) {
    if (err.code == 0) return;
    std::exception_ptr ex = make_error(err.code, error_message(err));
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

/** Views a borrowed byte run (null when empty) as an owned vector. */
inline std::vector<uint8_t> borrow_bytes(const uint8_t* ptr, size_t len) {
    if (ptr == nullptr || len == 0) return std::vector<uint8_t>();
    return std::vector<uint8_t>(ptr, ptr + len);
}

/** The data pointer of a string argument, as the C ABI expects it. */
inline const uint8_t* utf8(std::string_view s) noexcept {
    return reinterpret_cast<const uint8_t*>(s.data());
}

} // namespace detail

/**
 * Verifies that the linked {{LIBRARY}} library implements the C ABI revision
 * and the module contracts this header was generated against, throwing
 * LoadError naming the first mismatch. The check runs once; every free
 * function, constructor, and static member calls this before its first
 * native call, so calling it at startup is optional.
 */
inline void check_library() {
    static const std::string failure = [] {
        uint32_t abi = {{PREFIX}}_abi_version();
        if (abi != {{MACRO}}_ABI_VERSION) {
            return "{{LIBRARY}}: C ABI revision mismatch (header " + std::to_string({{MACRO}}_ABI_VERSION) +
                   ", library " + std::to_string(abi) + ")";
        }
{{CHECKSUMS}}        return std::string();
    }();
    if (!failure.empty()) throw LoadError(failure);
}

