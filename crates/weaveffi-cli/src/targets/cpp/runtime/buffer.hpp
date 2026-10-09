namespace detail {

/**
 * Serializes values into the value-buffer wire format: little-endian, packed
 * with no alignment, lengths and element counts as u32. The `write`
 * overloads below encode one value of each type.
 */
class BufferWriter {
public:
    /** Pointer to the encoded bytes. */
    const uint8_t* data() const noexcept { return buf_.data(); }

    /** Number of encoded bytes. */
    size_t size() const noexcept { return buf_.size(); }

    /** Appends one byte. */
    void byte(uint8_t v) { buf_.push_back(v); }

    /** Appends an unsigned integer, little-endian. */
    template <typename U>
    void le(U v) {
        for (size_t i = 0; i < sizeof(U); ++i) buf_.push_back(static_cast<uint8_t>(v >> (8 * i)));
    }

    /** Appends a byte length or an element count as a u32, failing with -3 past u32. */
    void len(size_t n) {
        if (static_cast<uint64_t>(n) > UINT32_MAX) throw Error(-3, "value buffer length exceeds u32");
        le(static_cast<uint32_t>(n));
    }

    /** Appends `n` raw bytes. */
    void bytes(const void* data, size_t n) {
        const uint8_t* p = static_cast<const uint8_t*>(data);
        buf_.insert(buf_.end(), p, p + n);
    }

private:
    std::vector<uint8_t> buf_;
};

/**
 * Decodes values from the value-buffer wire format. A malformed buffer is a
 * producer bug (both sides are generated from one API), so every decode
 * failure throws InternalError with the marshalling code -3. Object tokens
 * adopted before a failure live in wrappers that release them on unwind.
 */
class BufferReader {
public:
    /** Reads from the `len` bytes at `data` (null when empty). */
    BufferReader(const uint8_t* data, size_t len) noexcept : data_(data), len_(data != nullptr ? len : 0) {}

    /** Throws the marshalling failure (-3) for a malformed buffer. */
    [[noreturn]] static void fail(const char* what) {
        throw InternalError(-3, std::string("malformed value buffer: ") + what);
    }

    /** Bytes not yet consumed. */
    size_t remaining() const noexcept { return len_ - pos_; }

    /** Consumes `n` bytes, returning where they start. */
    const uint8_t* take(size_t n, const char* what) {
        if (remaining() < n) fail(what);
        const uint8_t* p = data_ + pos_;
        pos_ += n;
        return p;
    }

    /** Reads an unsigned integer, little-endian. */
    template <typename U>
    U le(const char* what) {
        const uint8_t* p = take(sizeof(U), what);
        U v = 0;
        for (size_t i = 0; i < sizeof(U); ++i) v = static_cast<U>(v | static_cast<U>(static_cast<U>(p[i]) << (8 * i)));
        return v;
    }

    /** Reads a 0/1 byte (a bool or an optional's presence flag). */
    bool flag(const char* what) {
        uint8_t b = le<uint8_t>(what);
        if (b > 1) fail(what);
        return b != 0;
    }

    /** Reads a byte length, rejecting one larger than the bytes remaining. */
    size_t len() {
        size_t n = le<uint32_t>("length");
        if (n > remaining()) fail("length prefix exceeds remaining buffer");
        return n;
    }

    /**
     * Reads an element count. Elements may encode to zero bytes, so a count
     * larger than the bytes remaining is legal; preallocate with reserve_hint().
     */
    size_t count() { return le<uint32_t>("count"); }

    /** A preallocation size for `n` elements that a malformed count can't inflate. */
    size_t reserve_hint(size_t n) const noexcept { return n < remaining() ? n : remaining(); }

    /** Rejects unconsumed bytes after decoding a complete value. */
    void expect_end() const {
        if (pos_ != len_) fail("trailing bytes after value");
    }

private:
    const uint8_t* data_;
    size_t len_;
    size_t pos_ = 0;
};

/** Selects a `read` overload by the type it decodes. */
template <typename T>
struct type_tag {};

/** Reads one `T` (declared below and per value type). */
template <typename T>
T read(BufferReader& r) {
    return read(r, type_tag<T>{});
}

inline void write(BufferWriter& w, bool v) { w.byte(v ? 1 : 0); }
inline void write(BufferWriter& w, int8_t v) { w.byte(static_cast<uint8_t>(v)); }
inline void write(BufferWriter& w, uint8_t v) { w.byte(v); }
inline void write(BufferWriter& w, int16_t v) { w.le(static_cast<uint16_t>(v)); }
inline void write(BufferWriter& w, uint16_t v) { w.le(v); }
inline void write(BufferWriter& w, int32_t v) { w.le(static_cast<uint32_t>(v)); }
inline void write(BufferWriter& w, uint32_t v) { w.le(v); }
inline void write(BufferWriter& w, int64_t v) { w.le(static_cast<uint64_t>(v)); }
inline void write(BufferWriter& w, uint64_t v) { w.le(v); }

inline void write(BufferWriter& w, float v) {
    uint32_t bits = 0;
    std::memcpy(&bits, &v, sizeof bits);
    w.le(bits);
}

inline void write(BufferWriter& w, double v) {
    uint64_t bits = 0;
    std::memcpy(&bits, &v, sizeof bits);
    w.le(bits);
}

inline void write(BufferWriter& w, std::string_view v) {
    w.len(v.size());
    w.bytes(v.data(), v.size());
}

inline void write(BufferWriter& w, const std::vector<uint8_t>& v) {
    w.len(v.size());
    w.bytes(v.data(), v.size());
}

inline bool read(BufferReader& r, type_tag<bool>) { return r.flag("bool"); }
inline int8_t read(BufferReader& r, type_tag<int8_t>) { return static_cast<int8_t>(r.le<uint8_t>("i8")); }
inline uint8_t read(BufferReader& r, type_tag<uint8_t>) { return r.le<uint8_t>("u8"); }
inline int16_t read(BufferReader& r, type_tag<int16_t>) { return static_cast<int16_t>(r.le<uint16_t>("i16")); }
inline uint16_t read(BufferReader& r, type_tag<uint16_t>) { return r.le<uint16_t>("u16"); }
inline int32_t read(BufferReader& r, type_tag<int32_t>) { return static_cast<int32_t>(r.le<uint32_t>("i32")); }
inline uint32_t read(BufferReader& r, type_tag<uint32_t>) { return r.le<uint32_t>("u32"); }
inline int64_t read(BufferReader& r, type_tag<int64_t>) { return static_cast<int64_t>(r.le<uint64_t>("i64")); }
inline uint64_t read(BufferReader& r, type_tag<uint64_t>) { return r.le<uint64_t>("u64"); }

inline float read(BufferReader& r, type_tag<float>) {
    uint32_t bits = r.le<uint32_t>("f32");
    float v = 0;
    std::memcpy(&v, &bits, sizeof v);
    return v;
}

inline double read(BufferReader& r, type_tag<double>) {
    uint64_t bits = r.le<uint64_t>("f64");
    double v = 0;
    std::memcpy(&v, &bits, sizeof v);
    return v;
}

inline std::string read(BufferReader& r, type_tag<std::string>) {
    size_t n = r.len();
    return std::string(reinterpret_cast<const char*>(r.take(n, "string")), n);
}

inline std::vector<uint8_t> read(BufferReader& r, type_tag<std::vector<uint8_t>>) {
    size_t n = r.len();
    const uint8_t* p = r.take(n, "bytes");
    return std::vector<uint8_t>(p, p + n);
}

/** A C-style enum crosses as its `int32_t` discriminant. */
template <typename E, std::enable_if_t<std::is_enum_v<E>, int> = 0>
void write(BufferWriter& w, E v) {
    write(w, static_cast<int32_t>(v));
}

template <typename E, std::enable_if_t<std::is_enum_v<E>, int> = 0>
E read(BufferReader& r, type_tag<E>) {
    return static_cast<E>(read<int32_t>(r));
}

/** Whether `T` is an interface wrapper (it names the C type it wraps). */
template <typename T, typename = void>
struct is_object : std::false_type {};

template <typename T>
struct is_object<T, std::void_t<typename T::raw_type>> : std::true_type {};

/**
 * An object crosses as a token: a fresh strong reference from the wrapper's
 * `_clone`, which the reader adopts while the writer's wrapper keeps its own.
 */
template <typename T, std::enable_if_t<is_object<T>::value, int> = 0>
void write(BufferWriter& w, const T& v) {
    write(w, static_cast<uint64_t>(reinterpret_cast<uintptr_t>(v.clone_handle())));
}

template <typename T, std::enable_if_t<is_object<T>::value, int> = 0>
T read(BufferReader& r, type_tag<T>) {
    auto* raw = reinterpret_cast<typename T::raw_type*>(static_cast<uintptr_t>(read<uint64_t>(r)));
    if (raw == nullptr) BufferReader::fail("null object token");
    return T(adopt, raw);
}

/** `T?`: a presence flag, then the value when present. */
template <typename T>
void write(BufferWriter& w, const std::optional<T>& v) {
    w.byte(v.has_value() ? 1 : 0);
    if (v.has_value()) write(w, *v);
}

template <typename T>
std::optional<T> read(BufferReader& r, type_tag<std::optional<T>>) {
    if (!r.flag("option flag")) return std::nullopt;
    return std::optional<T>(read<T>(r));
}

/** `[T]`: a u32 count, then the elements. */
template <typename T>
void write(BufferWriter& w, const std::vector<T>& v) {
    w.len(v.size());
    for (const auto& item : v) write(w, item);
}

template <typename T>
std::vector<T> read(BufferReader& r, type_tag<std::vector<T>>) {
    size_t n = r.count();
    std::vector<T> v;
    v.reserve(r.reserve_hint(n));
    for (size_t i = 0; i < n; ++i) v.push_back(read<T>(r));
    return v;
}

/** `{K: V}`: a u32 count, then each key and value. A repeated key is malformed. */
template <typename K, typename V>
void write(BufferWriter& w, const std::unordered_map<K, V>& v) {
    w.len(v.size());
    for (const auto& entry : v) {
        write(w, entry.first);
        write(w, entry.second);
    }
}

template <typename K, typename V>
std::unordered_map<K, V> read(BufferReader& r, type_tag<std::unordered_map<K, V>>) {
    size_t n = r.count();
    std::unordered_map<K, V> v;
    v.reserve(r.reserve_hint(n));
    for (size_t i = 0; i < n; ++i) {
        K key = read<K>(r);
        V value = read<V>(r);
        if (!v.emplace(std::move(key), std::move(value)).second) BufferReader::fail("repeated map key");
    }
    return v;
}

/** Encodes one value into a fresh buffer. */
template <typename T>
BufferWriter encode(const T& value) {
    BufferWriter w;
    write(w, value);
    return w;
}

/** Decodes one complete `T` from a borrowed buffer, rejecting trailing bytes. */
template <typename T>
T decode(const uint8_t* ptr, size_t len) {
    BufferReader r(ptr, len);
    T value = read<T>(r);
    r.expect_end();
    return value;
}

/** Decodes one complete `T` from a producer-returned buffer, then releases the buffer. */
template <typename T>
T take(const uint8_t* ptr, size_t len) {
    Run run{ptr, len};
    return decode<T>(ptr, len);
}

/** Hands a callback's buffered return to the producer as a {{PREFIX}}_alloc run. */
inline void give(const BufferWriter& value, uint8_t** out_ptr, size_t* out_len) {
    give_run(value.data(), value.size(), out_ptr, out_len);
}

/** Throws the error class `E` of a domain code with fields, decoding them from the error's payload. */
template <typename E, typename... Fields>
[[noreturn]] void raise_with_fields(const error& err) {
    BufferReader r(err.payload_ptr, err.payload_len);
    E e{message(err), read<Fields>(r)...};
    r.expect_end();
    throw e;
}

/**
 * Reports a domain exception from a callback: its code and message, and its
 * `fields` encoded as the payload. A field that can't be encoded reports a
 * plain failure (-1) instead.
 */
template <typename... Fields>
void report_with_fields(error* out_err, const Error& e, const Fields&... fields) noexcept {
    try {
        BufferWriter payload;
        (write(payload, fields), ...);
        set_error(out_err, e.code(), e.what());
        {{PREFIX}}_error_set_payload(out_err, payload.data(), payload.size());
    } catch (...) {
        set_error(out_err, -1, e.what());
    }
}

} // namespace detail
