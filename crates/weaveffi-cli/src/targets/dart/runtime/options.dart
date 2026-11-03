// ── Optional scalars ──
// An optional integer, float, bool, or C-style enum crosses by value as a
// `{{PREFIX}}_opt_{prim}`: a `bool has` and the value, `0` when absent. An
// optional enum is a `{{PREFIX}}_opt_i32` of its discriminant. Each struct
// below mirrors one, and each `_opt...` function builds one from a nullable
// Dart value.

final class _OptBool extends Struct {
  @Bool()
  external bool has;
  @Bool()
  external bool value;
}

final class _OptI8 extends Struct {
  @Bool()
  external bool has;
  @Int8()
  external int value;
}

final class _OptI16 extends Struct {
  @Bool()
  external bool has;
  @Int16()
  external int value;
}

final class _OptI32 extends Struct {
  @Bool()
  external bool has;
  @Int32()
  external int value;
}

final class _OptI64 extends Struct {
  @Bool()
  external bool has;
  @Int64()
  external int value;
}

final class _OptU8 extends Struct {
  @Bool()
  external bool has;
  @Uint8()
  external int value;
}

final class _OptU16 extends Struct {
  @Bool()
  external bool has;
  @Uint16()
  external int value;
}

final class _OptU32 extends Struct {
  @Bool()
  external bool has;
  @Uint32()
  external int value;
}

final class _OptU64 extends Struct {
  @Bool()
  external bool has;
  @Uint64()
  external int value;
}

final class _OptF32 extends Struct {
  @Bool()
  external bool has;
  @Float()
  external double value;
}

final class _OptF64 extends Struct {
  @Bool()
  external bool has;
  @Double()
  external double value;
}

_OptBool _optBool(bool? v) => Struct.create<_OptBool>()
  ..has = v != null
  ..value = v ?? false;

_OptI8 _optI8(int? v) => Struct.create<_OptI8>()
  ..has = v != null
  ..value = v ?? 0;

_OptI16 _optI16(int? v) => Struct.create<_OptI16>()
  ..has = v != null
  ..value = v ?? 0;

_OptI32 _optI32(int? v) => Struct.create<_OptI32>()
  ..has = v != null
  ..value = v ?? 0;

_OptI64 _optI64(int? v) => Struct.create<_OptI64>()
  ..has = v != null
  ..value = v ?? 0;

_OptU8 _optU8(int? v) => Struct.create<_OptU8>()
  ..has = v != null
  ..value = v ?? 0;

_OptU16 _optU16(int? v) => Struct.create<_OptU16>()
  ..has = v != null
  ..value = v ?? 0;

_OptU32 _optU32(int? v) => Struct.create<_OptU32>()
  ..has = v != null
  ..value = v ?? 0;

_OptU64 _optU64(int? v) => Struct.create<_OptU64>()
  ..has = v != null
  ..value = v ?? 0;

_OptF32 _optF32(double? v) => Struct.create<_OptF32>()
  ..has = v != null
  ..value = v ?? 0.0;

_OptF64 _optF64(double? v) => Struct.create<_OptF64>()
  ..has = v != null
  ..value = v ?? 0.0;
