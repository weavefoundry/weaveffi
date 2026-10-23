// ── Value buffers ──
// Records, rich enums, maps, error payloads, and every optional or list that
// isn't a scalar optional or a numeric array at a call boundary cross the C
// ABI serialized in this little-endian, packed format. A malformed buffer is
// a contract violation between producer and bindings, never a domain error.

Never _bufferError(String context) =>
    throw StateError('malformed value buffer: $context');

final class _BufferWriter {
  Uint8List _buf = Uint8List(64);
  late ByteData _data = ByteData.sublistView(_buf);
  int _len = 0;

  void _reserve(int extra) {
    if (_len + extra <= _buf.length) return;
    var cap = _buf.length * 2;
    while (cap < _len + extra) {
      cap *= 2;
    }
    _buf = Uint8List(cap)..setRange(0, _len, _buf);
    _data = ByteData.sublistView(_buf);
  }

  Uint8List takeBytes() => Uint8List.sublistView(_buf, 0, _len);

  void writeBool(bool v) => writeU8(v ? 1 : 0);

  void writeFlag(bool present) => writeU8(present ? 1 : 0);

  void writeI8(int v) {
    _reserve(1);
    _data.setInt8(_len, v);
    _len += 1;
  }

  void writeU8(int v) {
    _reserve(1);
    _data.setUint8(_len, v);
    _len += 1;
  }

  void writeI16(int v) {
    _reserve(2);
    _data.setInt16(_len, v, Endian.little);
    _len += 2;
  }

  void writeU16(int v) {
    _reserve(2);
    _data.setUint16(_len, v, Endian.little);
    _len += 2;
  }

  void writeI32(int v) {
    _reserve(4);
    _data.setInt32(_len, v, Endian.little);
    _len += 4;
  }

  void writeU32(int v) {
    _reserve(4);
    _data.setUint32(_len, v, Endian.little);
    _len += 4;
  }

  void writeI64(int v) {
    _reserve(8);
    _data.setInt64(_len, v, Endian.little);
    _len += 8;
  }

  // Dart ints are signed 64-bit; a u64 travels as its bit pattern.
  void writeU64(int v) => writeI64(v);

  void writeF32(double v) {
    _reserve(4);
    _data.setFloat32(_len, v, Endian.little);
    _len += 4;
  }

  void writeF64(double v) {
    _reserve(8);
    _data.setFloat64(_len, v, Endian.little);
    _len += 8;
  }

  void writeLength(int v) => writeU32(v);

  void writeString(String v) => writeBytes(utf8.encode(v));

  void writeBytes(List<int> v) {
    writeLength(v.length);
    _reserve(v.length);
    _buf.setRange(_len, _len + v.length, v);
    _len += v.length;
  }

  void writeOptional<T extends Object>(T? v, void Function(T) write) {
    writeFlag(v != null);
    if (v != null) write(v);
  }

  void writeList<T>(List<T> v, void Function(T) write) {
    writeLength(v.length);
    for (final e in v) {
      write(e);
    }
  }

  void writeMap<K, V>(
      Map<K, V> v, void Function(K) writeKey, void Function(V) writeValue) {
    writeLength(v.length);
    for (final e in v.entries) {
      writeKey(e.key);
      writeValue(e.value);
    }
  }
}

final class _BufferReader {
  _BufferReader(this._buf) : _data = ByteData.sublistView(_buf);

  final Uint8List _buf;
  final ByteData _data;
  int _pos = 0;

  int _take(int n, String context) {
    if (_buf.length - _pos < n) _bufferError('truncated $context');
    final at = _pos;
    _pos += n;
    return at;
  }

  bool readBool() {
    final b = _buf[_take(1, 'bool')];
    if (b > 1) _bufferError('bool byte $b');
    return b == 1;
  }

  bool readFlag() {
    final b = _buf[_take(1, 'option flag')];
    if (b > 1) _bufferError('option flag $b');
    return b == 1;
  }

  int readI8() => _data.getInt8(_take(1, 'i8'));

  int readU8() => _buf[_take(1, 'u8')];

  int readI16() => _data.getInt16(_take(2, 'i16'), Endian.little);

  int readU16() => _data.getUint16(_take(2, 'u16'), Endian.little);

  int readI32() => _data.getInt32(_take(4, 'i32'), Endian.little);

  int readU32() => _data.getUint32(_take(4, 'u32'), Endian.little);

  int readI64() => _data.getInt64(_take(8, 'i64'), Endian.little);

  // Dart ints are signed 64-bit; a u64 arrives as its bit pattern.
  int readU64() => readI64();

  double readF32() => _data.getFloat32(_take(4, 'f32'), Endian.little);

  double readF64() => _data.getFloat64(_take(8, 'f64'), Endian.little);

  int readLength() => readU32();

  String readString() {
    final n = readLength();
    final at = _take(n, 'string');
    return utf8.decode(Uint8List.sublistView(_buf, at, at + n));
  }

  Uint8List readBytes() {
    final n = readLength();
    final at = _take(n, 'bytes');
    return Uint8List.fromList(Uint8List.sublistView(_buf, at, at + n));
  }

  T? readOptional<T extends Object>(T Function() read) =>
      readFlag() ? read() : null;

  // Collections grow as they decode, so a corrupt count can't force a huge
  // allocation up front.
  List<T> readList<T>(T Function() read) =>
      <T>[for (var i = readLength(); i > 0; i--) read()];

  Map<K, V> readMap<K, V>(K Function() readKey, V Function() readValue) {
    final map = <K, V>{};
    for (var i = readLength(); i > 0; i--) {
      final key = readKey();
      if (map.containsKey(key)) _bufferError('duplicate map key $key');
      map[key] = readValue();
    }
    return map;
  }

  void expectEnd() {
    if (_pos != _buf.length) _bufferError('trailing bytes');
  }
}

/// Encodes one value with [write] into a fresh buffer.
Uint8List _encode<T>(T value, void Function(_BufferWriter, T) write) {
  final writer = _BufferWriter();
  write(writer, value);
  return writer.takeBytes();
}

/// Decodes exactly one value with [read] from [bytes].
T _decode<T>(Uint8List bytes, T Function(_BufferReader) read) {
  final reader = _BufferReader(bytes);
  final value = read(reader);
  reader.expectEnd();
  return value;
}

// Records and rich enums compare by value: lists, maps, and byte arrays
// element by element, and everything else with `==` (interface wrappers by
// identity).

bool _deepEquals(Object? a, Object? b) {
  if (identical(a, b)) return true;
  if (a is List && b is List) {
    if (a.length != b.length) return false;
    for (var i = 0; i < a.length; i++) {
      if (!_deepEquals(a[i], b[i])) return false;
    }
    return true;
  }
  if (a is Map && b is Map) {
    if (a.length != b.length) return false;
    for (final key in a.keys) {
      if (!b.containsKey(key) || !_deepEquals(a[key], b[key])) return false;
    }
    return true;
  }
  return a == b;
}

int _deepHash(Object? v) => switch (v) {
      List<Object?>() => Object.hashAll(v.map(_deepHash)),
      Map<Object?, Object?>() => Object.hashAllUnordered(
          v.entries.map((e) => Object.hash(_deepHash(e.key), _deepHash(e.value)))),
      _ => v.hashCode,
    };
