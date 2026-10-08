# frozen_string_literal: true

# Conformance consumer: codec sample, the wire oracle, Ruby target.
#
# The shared-vector loop: for every vector the producer serves, decode it,
# hand it back to `check_vector` (which re-encodes it), and push each
# primitive vector's value through the matching direct-family `echo_*`.
# Then vectors built from literals (so a symmetric encode/decode bug can't
# hide), spot checks of decoded fields (64-bit integers exact, float bits,
# astral text), the typed out-of-range error and its payload, malformed
# buffers sent through the raw attached function (each rejected with -3)
# and the encoder's own range checks, and object identity and reference
# counting through buffers. Ends with every leak counter at zero.

require_relative 'support'
require 'codec'

I64_MIN = -(2**63)
I64_MAX = (2**63) - 1
U64_MAX = (2**64) - 1

V = Codec::Vector

def f32_bits(x)
  [x].pack('e').unpack1('L<')
end

def f64_bits(x)
  [x].pack('E').unpack1('Q<')
end

# Floats compare bitwise, except that any NaN matches any NaN.
def same_float?(a, b)
  (a.nan? && b.nan?) || f64_bits(a) == f64_bits(b)
end

# Closes every object wrapper inside a decoded value.
def release(value)
  case value
  when Codec::Token then value.close
  when Array then value.each { |v| release(v) }
  when Hash then value.each_value { |v| release(v) }
  when Codec::Holder then [value.primary, value.spare, value.many, value.by_name].each { |v| release(v) }
  when V::Objects then release(value.value)
  end
end

# The direct-family echo of each primitive vector.
ECHOES = {
  V::I8 => :echo_i8,
  V::U8 => :echo_u8,
  V::I16 => :echo_i16,
  V::U16 => :echo_u16,
  V::I32 => :echo_i32,
  V::U32 => :echo_u32,
  V::I64 => :echo_i64,
  V::U64 => :echo_u64,
  V::F32 => :echo_f32,
  V::F64 => :echo_f64,
  V::Flag => :echo_bool,
  V::Text => :echo_text,
  V::Blob => :echo_blob,
  V::Hue => :echo_color
}.freeze

def holder(primary, spare: nil, many: [], by_name: {})
  Codec::Holder.new(primary: primary, spare: spare, many: many, by_name: by_name)
end

# Sends a raw (malformed) Vector encoding to `check_vector` through the
# attached C function, which must reject it as a marshalling failure.
def reject(bytes, what)
  err = Codec::ErrorStruct.new
  ok = Codec.codec_codec_check_vector(0, bytes, bytes.bytesize, err)
  code = err[:code]
  Codec.codec_error_clear(err)
  expect(!ok && code == -3, "#{what} is rejected with -3 (got #{code})")
end

def run_codec
  n = Codec.vector_count
  expect(n >= 60, "at least 60 vectors (got #{n})")
  names = Array.new(n) { |i| Codec.vector_name(i) }
  find = ->(name) { names.index(name) || raise("no vector named #{name}") }

  # 1. The loop.
  n.times do |i|
    v = Codec.vector(i)
    unless Codec.check_vector(i, v)
      raise "vector #{i} (#{names[i]}) did not round-trip; the producer saw #{Codec.describe_vector(v)}"
    end
    expect(!Codec.check_vector((i + 1) % n, v), "vector #{i} (#{names[i]}) never matches its neighbor")
    echo = ECHOES[v.class]
    if echo
      back = Codec.public_send(echo, v.value)
      same = v.value.is_a?(Float) ? same_float?(back, v.value) : back == v.value
      expect(same, "echo of vector #{i} (#{names[i]}): #{back.inspect} vs #{v.value.inspect}")
      expect(f32_bits(back) == f32_bits(v.value), "f32 bits of #{names[i]}") if v.is_a?(V::F32)
    end
    release(v)
  end

  # 2. Literal vectors.
  canonical = {
    i8_value: -8, u8_value: 200, i16_value: -16_000, u16_value: 60_000,
    i32_value: -2_000_000_000, u32_value: 4_000_000_000, i64_value: -9_007_199_254_740_993,
    u64_value: U64_MAX, f32_value: 1.5, f64_value: -2.25e100, flag: true, color: Codec::Color::BLUE
  }
  scalars = ->(fields) { V::AllScalars.new(value: Codec::Scalars.new(**fields)) }
  expect(Codec.check_vector(find['scalars canonical'], scalars[canonical]), 'scalars canonical')
  expect(!Codec.check_vector(find['scalars canonical'], scalars[canonical.merge(u16_value: 60_001)]),
         'scalars canonical with one field changed')
  labeled = V::Figure.new(value: Codec::Shape::Labeled.new(label: 'tag', count: 3))
  expect(Codec.check_vector(find['shape labeled'], labeled), 'shape labeled')
  expect(Codec.check_vector(find['string interior nul'], V::Text.new(value: "nul\0inside\0")), 'interior nul')
  other_nan = [0x7ff8000000000001].pack('Q<').unpack1('E')
  expect(Codec.check_vector(find['f64 nan'], V::F64.new(value: other_nan)), 'any NaN is the NaN vector')
  expect(Codec.check_vector(find['f64 -0'], V::F64.new(value: -0.0)), 'negative zero')
  expect(!Codec.check_vector(find['f64 -0'], V::F64.new(value: 0.0)), 'positive zero is not negative zero')
  expect(Codec.check_vector(find['u64 max'], V::U64.new(value: U64_MAX)), 'u64 max')
  expect(Codec.check_vector(find['enum infrared'], V::Hue.new(value: Codec::Color::INFRARED)), 'enum infrared')
  expect(Codec.check_vector(find['optional zero'], V::MaybeI64.new(value: 0)), 'optional zero')
  expect(Codec.check_vector(find['optional absent'], V::MaybeI64.new(value: nil)), 'optional absent')
  expect(!Codec.check_vector(find['optional zero'], V::MaybeI64.new(value: nil)), 'absent is not zero')
  reordered = V::Counts.new(value: { 'x' => 0, 'héllo' => -1, '' => I64_MAX })
  expect(Codec.check_vector(find['map of strings'], reordered), 'a map built in another order')
  expect(Codec.check_vector(find['blank'], V::Blank.new), 'blank')
  lone = Codec::Token.new(-1)
  expect(Codec.check_vector(find['objects sparse'], V::Objects.new(value: holder(lone))),
         'objects sparse from a consumer-made token')
  lone.close

  # 3. Spot checks on decoded values.
  expect(Codec.vector(find['i64 past 2^53']) == V::I64.new(value: -9_007_199_254_740_993), 'i64 past 2^53 is exact')
  subnormal = Codec.vector(find['f32 min subnormal'])
  expect(subnormal.is_a?(V::F32) && f32_bits(subnormal.value) == 1, 'f32 min subnormal bits')
  astral = Codec.vector(find['string astral'])
  expect(astral == V::Text.new(value: '🦀 crab 😀') && astral.value.encoding == Encoding::UTF_8, 'string astral')
  m = Codec.vector(find['scalars minimum']).value
  expect([m.i8_value, m.i16_value, m.i32_value, m.i64_value, m.u64_value] ==
         [-128, -32_768, -(2**31), I64_MIN, 0], 'scalars minimum integers')
  expect(m.f32_value == -Float::INFINITY && m.f64_value.nan?, 'scalars minimum floats')
  expect(m.color == Codec::Color::INFRARED && m.flag == false, 'scalars minimum enum and flag')
  deep = Codec.vector(find['composite canonical'])
  expect(deep.is_a?(V::Deep), 'composite canonical is Deep')
  c = deep.value
  expect(c.name == 'héllo wörld ✓', 'composite name')
  expect(c.blob == [0, 1, 2, 253, 254, 255].pack('C*') && c.blob.encoding == Encoding::BINARY, 'composite blob')
  expect(c.some_i64 == I64_MIN && c.none_i64.nil? && c.some_text == '', 'composite optionals')
  expect(c.names == ['a', '', 'ccc'], 'composite names')
  expect(c.matrix == [[1, 2, 3], [], [-4]], 'composite matrix')
  expect(c.floats.length == 6 && c.floats[0].nan? && f64_bits(c.floats[3]) == f64_bits(-0.0) &&
         c.floats[4] == 5e-324, 'composite floats')
  expect(c.by_name == { 'one' => 1, 'two' => 2, 'neg' => -3, '' => I64_MAX }, 'composite by_name')
  expect(c.by_id.keys.sort == [-1, 42, (2**31) - 1] && c.by_id[42].u64_value.zero?, 'composite by_id')
  expect(c.by_color == { Codec::Color::INFRARED => 'below', Codec::Color::BLUE => 'sky' }, 'composite by_color')
  expect(c.flags == { 0 => false, U64_MAX => true }, 'composite flags')
  expect(c.scalars.u32_value == 4_000_000_000, 'composite scalars')
  expect(c.shape == Codec::Shape::Labeled.new(label: 'tag', count: 3), 'composite shape')
  expect(c.shapes.length == 6 && c.shapes[5].is_a?(Codec::Shape::Nested) && c.shapes[5].note.nil?, 'composite shapes')
  expect(c.maybe_shape.is_a?(Codec::Shape::Nested), 'composite maybe_shape')
  expect(c.maybe_list == "\x09\x08".b, 'composite maybe_list')
  expect(c.sparse == [true, nil, false], 'composite sparse')
  expect(c.colors == [0, 1, 7, -1], 'composite colors')

  # 4. The typed error with its payload.
  e = expect_raise(Codec::CodecError::OutOfRange, 'vector past the end') { Codec.vector(n) }
  expect(e.is_a?(Codec::CodecError) && e.is_a?(Codec::Error) && e.code == 1, 'OutOfRange is the domain error')
  expect(e.index == n && e.count == n, "OutOfRange payload (got #{e.index}, #{e.count})")
  expect(e.message == "vector #{n} is out of range (count #{n})", "OutOfRange message (got #{e.message})")
  e = expect_raise(Codec::CodecError::OutOfRange, 'vector_name past the end') { Codec.vector_name(n + 5) }
  expect(e.index == n + 5 && e.count == n, 'vector_name payload')
  expect(!Codec.check_vector(n, V::Blank.new), 'check_vector past the end is false')

  # 5. Malformed input: raw buffers the encoder would never produce reach
  # the producer through the attached C function; an undeclared enum value
  # through the direct family traps; the encoder refuses what the wire
  # can't carry.
  str = ->(s) { [s.bytesize].pack('L<') + s.b }
  reject([V::I8::TAG].pack('l<'), 'a truncated buffer')
  reject([99].pack('l<'), 'an unknown tag')
  reject([V::Blank::TAG].pack('l<') + "\x00".b, 'trailing bytes')
  reject([V::Flag::TAG].pack('l<') + "\x02".b, 'a bool byte of 2')
  reject([V::Hue::TAG, 3].pack('l<l<'), 'an undeclared enum value')
  reject([V::Counts::TAG, 2].pack('l<L<') + str['a'] + [1].pack('q<') + str['a'] + [2].pack('q<'),
         'a duplicate map key')
  reject([V::Text::TAG].pack('l<') + str["\xC3\x28".b], 'invalid UTF-8')
  bug = expect_raise(Codec::NativeBugError, 'an undeclared enum value') { Codec.echo_color(3) }
  expect(bug.code == -3 && bug.message.include?('code -3'), "the trap names the code (got #{bug.message})")
  expect_raise(RangeError, 'an i64 out of range') { Codec.check_vector(0, V::I64.new(value: 2**64)) }
  expect_raise(RangeError, 'a u8 out of range') { Codec.check_vector(0, V::U8.new(value: -1)) }
  expect_raise(TypeError, 'a nil string') { Codec.echo_text(nil) }
  expect_raise(TypeError, 'a variant of another type') { Codec.check_vector(0, Codec::Shape::Empty.new) }

  # 6. Objects.
  full = Codec.vector(find['objects full'])
  expect(full.is_a?(V::Objects), 'objects full is Objects')
  h = full.value
  expect(h.primary.is_a?(Codec::Token) && h.primary.value == 10, 'primary token')
  expect(h.spare.value == 11, 'spare token')
  expect(h.many.map(&:value) == [12, 13, I64_MIN], 'many tokens')
  expect(h.by_name.transform_values(&:value) == { 'a' => 20, 'b' => 21 }, 'tokens by name')
  # Each encoding mints fresh references, so the holder can be sent twice.
  total = 10 + 11 + 12 + 13 + 20 + 21 + I64_MIN
  expect(Codec.sum_holder(h) == total && Codec.sum_holder(h) == total, 'sum_holder twice')
  # primary_of returns the very same object (a new wrapper of it).
  p = Codec.primary_of(h)
  expect(Codec.same_primary(h, holder(p)), 'primary_of is the same object')
  twin = Codec::Token.new(10)
  expect(!Codec.same_primary(h, holder(twin)), 'an equal value is not the same object')
  minus4 = Codec::Token.new(-4)
  shared = holder(twin, spare: twin, many: [twin, twin, minus4], by_name: { 'k' => twin })
  expect(Codec.sum_holder(shared) == 46, 'a holder of consumer tokens, one wrapper in several slots')
  expect_raise(TypeError, 'a wrapper of the wrong type') { Codec.sum_holder(holder('not a token')) }
  [p, twin, minus4].each(&:close)
  release(full)
  closed = expect_raise(Codec::Error, 'a closed token') { p.value }
  expect(closed.message.include?('used after close'), closed.message)
  n
end

count = nil
run_and_check_leaks(Codec) { count = run_codec }

puts "ruby/codec: OK (#{count} vectors)"
