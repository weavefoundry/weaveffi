//! The private runtime prelude of the generated header: the generic error
//! surface, the value-buffer reader/writer, and the listener registry.

use weaveffi_core::codegen::CodeWriter;

/// Emit the generic `WeaveFFIError` plus the `detail::check`/`detail::make_error`
/// helpers every non-throwing wrapper uses. A nonzero code on a non-throwing
/// callable can only be a producer panic or a marshalling failure, so it
/// surfaces as this generic exception rather than a typed domain error.
pub(crate) fn render_generic_error(out: &mut String, prefix: &str) {
    let mut w = CodeWriter::four_space();
    w.line("/** Base exception for every error reported through the C ABI. */");
    w.line("class WeaveFFIError : public std::runtime_error {");
    w.scope(|w| {
        w.line("int32_t code_;");
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        w.line("WeaveFFIError(int32_t code, const std::string& msg) : std::runtime_error(msg), code_(code) {}");
        w.line("int32_t code() const { return code_; }");
    });
    w.line("};");
    w.blank();

    w.line("namespace detail {");
    w.blank();
    w.line("/** Throw the generic WeaveFFIError if `err` carries a nonzero code. */");
    w.line(format!("inline void check({prefix}_error& err) {{"));
    w.scope(|w| {
        w.line("if (err.code == 0) return;");
        w.line("std::string msg(err.message ? err.message : \"unknown error\");");
        w.line("int32_t code = err.code;");
        w.line(format!("{prefix}_error_clear(&err);"));
        w.line("throw WeaveFFIError(code, msg);");
    });
    w.line("}");
    w.blank();
    w.line("/** Wrap an async-callback error as the generic WeaveFFIError. */");
    w.line("inline std::exception_ptr make_error(int32_t code, const std::string& msg) {");
    w.scope(|w| {
        w.line("return std::make_exception_ptr(WeaveFFIError(code, msg));");
    });
    w.line("}");
    w.blank();
    w.line("} // namespace detail");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the private value-buffer runtime: a writer and reader implementing
/// the WeaveFFI wire format (little-endian, packed, `u32` lengths), plus a
/// scope guard that releases producer-allocated buffers. A malformed buffer
/// is a producer/consumer contract violation, so decode failures throw the
/// generic `WeaveFFIError` (the producer-panic channel), never a typed
/// domain error.
pub(crate) fn render_buffer_runtime(out: &mut String, prefix: &str) {
    let body = r#"namespace detail {

/**
 * Serializes values into the WeaveFFI value-buffer wire format: little-endian,
 * packed with no alignment, lengths and element counts as u32.
 */
class BufferWriter {
    std::vector<uint8_t> buf_;

    template <typename T>
    void append_le(T v) {
        for (size_t i = 0; i < sizeof(T); ++i) {
            buf_.push_back(static_cast<uint8_t>(v >> (8 * i)));
        }
    }

public:
    /** Pointer to the encoded bytes. */
    const uint8_t* data() const { return buf_.data(); }

    /** Number of encoded bytes. */
    size_t size() const { return buf_.size(); }

    void write_bool(bool v) { buf_.push_back(v ? 1 : 0); }
    void write_i8(int8_t v) { buf_.push_back(static_cast<uint8_t>(v)); }
    void write_u8(uint8_t v) { buf_.push_back(v); }
    void write_i16(int16_t v) { append_le(static_cast<uint16_t>(v)); }
    void write_u16(uint16_t v) { append_le(v); }
    void write_i32(int32_t v) { append_le(static_cast<uint32_t>(v)); }
    void write_u32(uint32_t v) { append_le(v); }
    void write_i64(int64_t v) { append_le(static_cast<uint64_t>(v)); }
    void write_u64(uint64_t v) { append_le(v); }

    void write_f32(float v) {
        uint32_t bits = 0;
        std::memcpy(&bits, &v, sizeof(bits));
        append_le(bits);
    }

    void write_f64(double v) {
        uint64_t bits = 0;
        std::memcpy(&bits, &v, sizeof(bits));
        append_le(bits);
    }

    /** Writes a string, byte-buffer, or collection length as a u32. */
    void write_len(size_t n) { append_le(static_cast<uint32_t>(n)); }

    void write_string(const std::string& v) {
        write_len(v.size());
        buf_.insert(buf_.end(), v.begin(), v.end());
    }

    void write_bytes(const std::vector<uint8_t>& v) {
        write_len(v.size());
        buf_.insert(buf_.end(), v.begin(), v.end());
    }

    /** Writes an optional's presence flag: 0 absent, 1 present. */
    void write_option_flag(bool present) { buf_.push_back(present ? 1 : 0); }
};

/**
 * Decodes values from the WeaveFFI value-buffer wire format. A malformed
 * buffer is a producer/consumer contract violation (both sides are generated
 * from one IDL), so every decode failure throws the generic WeaveFFIError,
 * the same channel as a producer panic.
 */
class BufferReader {
    const uint8_t* data_;
    size_t len_;
    size_t pos_;

    [[noreturn]] static void fail(const char* what) {
        throw WeaveFFIError(-2, std::string("malformed WeaveFFI value buffer: ") + what);
    }

    void require(size_t n, const char* what) const {
        if (len_ - pos_ < n) fail(what);
    }

    template <typename T>
    T read_le(const char* what) {
        require(sizeof(T), what);
        uint64_t v = 0;
        for (size_t i = 0; i < sizeof(T); ++i) {
            v |= static_cast<uint64_t>(data_[pos_ + i]) << (8 * i);
        }
        pos_ += sizeof(T);
        return static_cast<T>(v);
    }

public:
    BufferReader(const uint8_t* data, size_t len) : data_(data), len_(len), pos_(0) {}

    /** Bytes not yet consumed. */
    size_t remaining() const { return len_ - pos_; }

    bool read_bool() {
        uint8_t b = read_le<uint8_t>("bool");
        if (b > 1) fail("bool byte out of range");
        return b != 0;
    }

    int8_t read_i8() { return read_le<int8_t>("i8"); }
    uint8_t read_u8() { return read_le<uint8_t>("u8"); }
    int16_t read_i16() { return read_le<int16_t>("i16"); }
    uint16_t read_u16() { return read_le<uint16_t>("u16"); }
    int32_t read_i32() { return read_le<int32_t>("i32"); }
    uint32_t read_u32() { return read_le<uint32_t>("u32"); }
    int64_t read_i64() { return read_le<int64_t>("i64"); }
    uint64_t read_u64() { return read_le<uint64_t>("u64"); }

    float read_f32() {
        uint32_t bits = read_le<uint32_t>("f32");
        float v = 0;
        std::memcpy(&v, &bits, sizeof(v));
        return v;
    }

    double read_f64() {
        uint64_t bits = read_le<uint64_t>("f64");
        double v = 0;
        std::memcpy(&v, &bits, sizeof(v));
        return v;
    }

    /** Reads a length prefix, rejecting one larger than the bytes remaining. */
    size_t read_len() {
        uint32_t n = read_le<uint32_t>("length");
        if (static_cast<size_t>(n) > remaining()) fail("length prefix exceeds remaining buffer");
        return static_cast<size_t>(n);
    }

    std::string read_string() {
        size_t n = read_len();
        std::string v(reinterpret_cast<const char*>(data_) + pos_, n);
        pos_ += n;
        return v;
    }

    std::vector<uint8_t> read_bytes() {
        size_t n = read_len();
        std::vector<uint8_t> v(data_ + pos_, data_ + pos_ + n);
        pos_ += n;
        return v;
    }

    bool read_option_flag() {
        uint8_t b = read_le<uint8_t>("option flag");
        if (b > 1) fail("option flag byte out of range");
        return b != 0;
    }

    /** Rejects unconsumed bytes after decoding a complete value. */
    void expect_end() const {
        if (pos_ != len_) fail("trailing bytes after value");
    }
};

/** Releases a producer-allocated buffer with @PREFIX@_free_bytes on scope exit. */
struct BufferGuard {
    /** The producer-allocated buffer, or null when the call reported an error. */
    const uint8_t* ptr;
    /** The buffer length in bytes. */
    size_t len;

    ~BufferGuard() {
        if (ptr != nullptr) @PREFIX@_free_bytes(const_cast<uint8_t*>(ptr), len);
    }
};

} // namespace detail

"#;
    out.push_str(&body.replace("@PREFIX@", prefix));
}

/// Emit the `detail` registry that pins each listener's heap-boxed
/// `std::function` (type-erased) until unregistration, plus the mutex
/// guarding it. Listener closures are threaded through the C `context`
/// pointer, so the box must outlive the registration.
pub(crate) fn render_listener_registry(out: &mut String) {
    out.push_str("namespace detail {\n\n");
    out.push_str("inline std::mutex& wv_listener_mutex() {\n");
    out.push_str("    static std::mutex m;\n");
    out.push_str("    return m;\n");
    out.push_str("}\n\n");
    out.push_str(
        "inline std::unordered_map<uint64_t, std::shared_ptr<void>>& wv_listener_registry() {\n",
    );
    out.push_str("    static std::unordered_map<uint64_t, std::shared_ptr<void>> registry;\n");
    out.push_str("    return registry;\n");
    out.push_str("}\n\n");
    out.push_str("} // namespace detail\n\n");
}
