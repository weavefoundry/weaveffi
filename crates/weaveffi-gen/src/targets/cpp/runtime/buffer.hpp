namespace detail {

/**
 * Serializes values into the value-buffer wire format: little-endian, packed
 * with no alignment, lengths and element counts as u32.
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
    const uint8_t* data() const noexcept { return buf_.data(); }

    /** Number of encoded bytes. */
    size_t size() const noexcept { return buf_.size(); }

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

    /** Writes a byte length or an element count as a u32. */
    void write_len(size_t n) {
        if (static_cast<uint64_t>(n) > UINT32_MAX) throw Error(-3, "value buffer length exceeds u32");
        append_le(static_cast<uint32_t>(n));
    }

    void write_string(std::string_view v) {
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
 * Decodes values from the value-buffer wire format. A malformed buffer is a
 * producer/consumer contract violation (both sides are generated from one
 * API), so every decode failure throws the generic Error with the
 * marshalling code -3. Object tokens adopted before a failure live in RAII
 * wrappers that release them on unwind.
 */
class BufferReader {
    const uint8_t* data_;
    size_t len_;
    size_t pos_;

    [[noreturn]] static void fail(const char* what) {
        throw Error(-3, std::string("malformed value buffer: ") + what);
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
    BufferReader(const uint8_t* data, size_t len) noexcept : data_(data), len_(data != nullptr ? len : 0), pos_(0) {}

    /** Bytes not yet consumed. */
    size_t remaining() const noexcept { return len_ - pos_; }

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

    /** Reads a byte length, rejecting one larger than the bytes remaining. */
    size_t read_len() {
        uint32_t n = read_le<uint32_t>("length");
        if (static_cast<size_t>(n) > remaining()) fail("length prefix exceeds remaining buffer");
        return static_cast<size_t>(n);
    }

    /**
     * Reads an element count. Elements may encode to zero bytes, so a count
     * larger than the bytes remaining is legal; callers cap preallocation
     * with reserve_hint().
     */
    size_t read_count() { return static_cast<size_t>(read_le<uint32_t>("count")); }

    /** A preallocation size for `n` elements that a malformed count can't inflate. */
    size_t reserve_hint(size_t n) const noexcept { return n < remaining() ? n : remaining(); }

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

/** Releases a producer-allocated buffer with {{PREFIX}}_free_bytes on scope exit. */
struct BufferGuard {
    /** The producer-allocated buffer, or null when the call reported an error. */
    const uint8_t* ptr;
    /** The buffer length in bytes. */
    size_t len;

    BufferGuard(const uint8_t* p, size_t n) noexcept : ptr(p), len(n) {}
    BufferGuard(const BufferGuard&) = delete;
    BufferGuard& operator=(const BufferGuard&) = delete;

    ~BufferGuard() {
        if (ptr != nullptr) {{PREFIX}}_free_bytes(const_cast<uint8_t*>(ptr), len);
    }
};

} // namespace detail

