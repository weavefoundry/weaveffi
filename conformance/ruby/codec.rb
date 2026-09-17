# frozen_string_literal: true
# Conformance consumer: codec sample, Ruby target.
#
# Round-trip oracle for the generated value-buffer codec. For each fixture the
# producer encodes and Ruby decodes (`sample_*`, checked field by field), Ruby
# encodes and the producer decodes (`verify_*` must return true), and both
# directions compose (`roundtrip_*` must equal the input). Ruby-built values
# cover the edges: empty strings, lists, and maps; non-ASCII text; i64/u64
# extremes; Float::NAN, +/-Float::INFINITY, and -0.0; every Shape variant; and
# objects inside buffers (Holder: a Token field, an optional Token, and a list
# of Tokens; `primary_of` returns a wrapper to the SAME object as
# `holder.primary`; every wrapper closes exactly once). Exit is non-zero on
# any mismatch. The cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "codec"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

def negative_zero?(x)
  x.zero? && (1.0 / x) == -Float::INFINITY
end

I64_MIN = -(2**63)
I64_MAX = 2**63 - 1
U64_MAX = 2**64 - 1

expect(Codec::ABI_VERSION == 2, "bindings target ABI revision 2")
expect(Codec::Color::RED.zero? && Codec::Color::GREEN == 1 && Codec::Color::BLUE == 7, "Color constants")

# --- Scalars -------------------------------------------------------------
scalars = Codec.sample_scalars
expect(scalars.is_a?(Codec::Scalars), "sample_scalars returns a Scalars")
expect(scalars.i8_value == -8, "i8 (got #{scalars.i8_value})")
expect(scalars.u8_value == 200, "u8 (got #{scalars.u8_value})")
expect(scalars.i16_value == -16_000, "i16 (got #{scalars.i16_value})")
expect(scalars.u16_value == 60_000, "u16 (got #{scalars.u16_value})")
expect(scalars.i32_value == -2_000_000_000, "i32 (got #{scalars.i32_value})")
expect(scalars.u32_value == 4_000_000_000, "u32 (got #{scalars.u32_value})")
expect(scalars.i64_value == -9_007_199_254_740_993, "i64 beyond 2^53 (got #{scalars.i64_value})")
expect(scalars.u64_value == U64_MAX, "u64::MAX (got #{scalars.u64_value})")
expect(scalars.f32_value == 1.5, "f32 (got #{scalars.f32_value})")
expect(scalars.f64_value == -2.25e100, "f64 (got #{scalars.f64_value})")
expect(scalars.flag == true, "flag")
expect(scalars.color == Codec::Color::BLUE, "color BLUE (got #{scalars.color})")

expect(Codec.verify_scalars(scalars) == true, "producer accepts the re-encoded Scalars")
expect(Codec.roundtrip_scalars(scalars) == scalars, "roundtrip_scalars equals the sample")

# The same fixture built from scratch in Ruby verifies; one changed field is a
# typed Mismatch (code 1).
rebuilt = Codec::Scalars.new(
  i8_value: -8, u8_value: 200, i16_value: -16_000, u16_value: 60_000,
  i32_value: -2_000_000_000, u32_value: 4_000_000_000,
  i64_value: -9_007_199_254_740_993, u64_value: U64_MAX,
  f32_value: 1.5, f64_value: -2.25e100, flag: true, color: Codec::Color::BLUE
)
expect(rebuilt == scalars, "Ruby-built Scalars equals the decoded sample")
expect(Codec.verify_scalars(rebuilt) == true, "Ruby-built Scalars verifies")
changed = Codec::Scalars.new(
  i8_value: -8, u8_value: 200, i16_value: -16_000, u16_value: 60_000,
  i32_value: -2_000_000_000, u32_value: 4_000_000_000,
  i64_value: -9_007_199_254_740_993, u64_value: U64_MAX,
  f32_value: 1.5, f64_value: -2.25e100, flag: false, color: Codec::Color::BLUE
)
begin
  Codec.verify_scalars(changed)
  raise "expected CodecError::Mismatch"
rescue Codec::CodecError::Mismatch => e
  expect(e.code == 1, "Mismatch code == 1 (got #{e.code})")
  expect(e.is_a?(Codec::CodecError) && e.is_a?(Codec::Error), "Mismatch subclasses the domain and brand errors")
end

# Every integer extreme plus float specials round-trip bit for bit.
extremes = Codec::Scalars.new(
  i8_value: -128, u8_value: 255, i16_value: -32_768, u16_value: 65_535,
  i32_value: -(2**31), u32_value: 2**32 - 1,
  i64_value: I64_MIN, u64_value: U64_MAX,
  f32_value: -Float::INFINITY, f64_value: -0.0, flag: false, color: Codec::Color::RED
)
back = Codec.roundtrip_scalars(extremes)
expect(back == extremes, "extreme Scalars round-trip")
expect(back.i64_value == I64_MIN && back.u64_value == U64_MAX, "64-bit extremes inside a record")
expect(back.f32_value == -Float::INFINITY, "f32 -inf inside a record")
expect(negative_zero?(back.f64_value), "f64 -0.0 keeps its sign inside a record")
highs = Codec::Scalars.new(
  i8_value: 127, u8_value: 0, i16_value: 32_767, u16_value: 0,
  i32_value: 2**31 - 1, u32_value: 0, i64_value: I64_MAX, u64_value: 0,
  f32_value: Float::NAN, f64_value: Float::INFINITY, flag: true, color: Codec::Color::GREEN
)
back = Codec.roundtrip_scalars(highs)
expect(back.i8_value == 127 && back.i16_value == 32_767 && back.i32_value == 2**31 - 1, "signed maxima")
expect(back.i64_value == I64_MAX, "i64::MAX inside a record")
expect(back.u8_value.zero? && back.u16_value.zero? && back.u32_value.zero? && back.u64_value.zero?, "unsigned zeros")
expect(back.f32_value.nan?, "f32 NaN survives inside a record")
expect(back.f64_value == Float::INFINITY, "f64 +inf inside a record")
expect(back.color == Codec::Color::GREEN, "enum inside a record")

# --- Composite -----------------------------------------------------------
composite = Codec.sample_composite
expect(composite.is_a?(Codec::Composite), "sample_composite returns a Composite")
expect(composite.name == "héllo wörld ✓", "unicode name (got #{composite.name.inspect})")
expect(composite.name.encoding == Encoding::UTF_8, "strings decode as UTF-8")
expect(composite.blob == "\x00\x01\x02\xFD\xFE\xFF".b, "blob bytes (got #{composite.blob.inspect})")
expect(composite.blob.encoding == Encoding::BINARY, "bytes decode as BINARY")
expect(composite.some_i64 == I64_MIN, "present optional i64::MIN (got #{composite.some_i64})")
expect(composite.none_i64.nil?, "absent optional is nil")
expect(composite.some_text == "", "present optional empty string (got #{composite.some_text.inspect})")
expect(composite.names == ["a", "", "ccc"], "list of strings (got #{composite.names})")
expect(composite.matrix == [[1, 2, 3], [], [-4]], "list of lists (got #{composite.matrix})")
expect(composite.empty == [], "empty list")
expect(composite.by_name == { "one" => 1, "two" => 2, "neg" => -3 }, "string-keyed map (got #{composite.by_name})")
expect(composite.by_id.keys.sort == [-1, 42], "int-keyed map keys (got #{composite.by_id.keys})")
expect(composite.by_id[-1] == scalars, "by_id[-1] is the canonical Scalars")
expect(composite.by_id[42].flag == false && composite.by_id[42].u64_value == U64_MAX, "by_id[42] differs only in flag")
expect(composite.scalars == scalars, "nested record equals sample_scalars")
expect(composite.shape == Codec::Shape::Labeled.new(label: "tag", count: 3), "rich enum field")
expect(composite.shape.tag == 3 && composite.shape.tag == Codec::Shape::Labeled::TAG, "variant tag")
expect(composite.shapes.length == 5, "one shape per variant")
expect(composite.shapes[0] == Codec::Shape::Empty.new, "shapes[0] Empty")
expect(composite.shapes[1] == Codec::Shape::Circle.new(radius: 2.5), "shapes[1] Circle")
expect(composite.shapes[2] == Codec::Shape::Rect.new(width: 1.0, height: 0.5), "shapes[2] Rect")
expect(composite.shapes[3] == Codec::Shape::Labeled.new(label: "", count: -1), "shapes[3] Labeled")
expect(composite.shapes[4] == Codec::Shape::Nested.new(inner: scalars, note: "n"), "shapes[4] Nested")
expect(composite.shapes.map(&:tag) == [0, 1, 2, 3, 4], "variant tags in order")
expect(composite.maybe_shape == Codec::Shape::Nested.new(inner: scalars, note: nil), "optional rich enum")
expect(composite.maybe_list == "\x09\x08".b, "optional bytes list (got #{composite.maybe_list.inspect})")
expect(composite.sparse == [true, nil, false], "list of optionals (got #{composite.sparse})")
expect(composite.colors == [Codec::Color::RED, Codec::Color::GREEN, Codec::Color::BLUE], "list of enums")

expect(Codec.verify_composite(composite) == true, "producer accepts the re-encoded Composite")
expect(Codec.roundtrip_composite(composite) == composite, "roundtrip_composite equals the sample")
described = Codec.describe_composite(composite)
expect(described.include?("héllo wörld ✓") && described.include?("Labeled"), "describe_composite renders (got #{described[0, 60].inspect})")

# Rebuilt in Ruby with hashes in a different insertion order: maps are
# unordered on the wire, so the producer still sees the canonical value.
rebuilt_composite = Codec::Composite.new(
  name: "héllo wörld ✓",
  blob: [0, 1, 2, 253, 254, 255].pack("C*"),
  some_i64: I64_MIN,
  none_i64: nil,
  some_text: "",
  names: ["a", "", "ccc"],
  matrix: [[1, 2, 3], [], [-4]],
  empty: [],
  by_name: { "two" => 2, "neg" => -3, "one" => 1 },
  by_id: { 42 => changed, -1 => rebuilt },
  scalars: rebuilt,
  shape: Codec::Shape::Labeled.new(label: "tag", count: 3),
  shapes: [
    Codec::Shape::Empty.new,
    Codec::Shape::Circle.new(radius: 2.5),
    Codec::Shape::Rect.new(width: 1.0, height: 0.5),
    Codec::Shape::Labeled.new(label: "", count: -1),
    Codec::Shape::Nested.new(inner: rebuilt, note: "n")
  ],
  maybe_shape: Codec::Shape::Nested.new(inner: rebuilt, note: nil),
  maybe_list: "\x09\x08".b,
  sparse: [true, nil, false],
  colors: [Codec::Color::RED, Codec::Color::GREEN, Codec::Color::BLUE]
)
expect(rebuilt_composite == composite, "Ruby-built Composite equals the decoded sample")
expect(Codec.verify_composite(rebuilt_composite) == true, "Ruby-built Composite verifies")
tweaked = Codec::Composite.new(
  name: composite.name, blob: composite.blob, some_i64: composite.some_i64, none_i64: nil,
  some_text: composite.some_text, names: composite.names, matrix: composite.matrix, empty: [],
  by_name: composite.by_name, by_id: composite.by_id, scalars: composite.scalars,
  shape: composite.shape, shapes: composite.shapes, maybe_shape: composite.maybe_shape,
  maybe_list: composite.maybe_list, sparse: [true, true, false], colors: composite.colors
)
begin
  Codec.verify_composite(tweaked)
  raise "expected CodecError::Mismatch for a tweaked Composite"
rescue Codec::CodecError::Mismatch => e
  expect(e.code == 1, "tweaked Composite -> Mismatch (got #{e.code})")
end

# An edge-value Composite: everything empty or absent, plus float specials in
# a list and nested extremes.
edge = Codec::Composite.new(
  name: "",
  blob: "".b,
  some_i64: I64_MAX,
  none_i64: -1,
  some_text: nil,
  names: [],
  matrix: [[], [-(2**31), 0, 2**31 - 1]],
  empty: [1.5, Float::INFINITY, -Float::INFINITY, -0.0, 0.0],
  by_name: {},
  by_id: { 0 => extremes },
  scalars: extremes,
  shape: Codec::Shape::Empty.new,
  shapes: [],
  maybe_shape: nil,
  maybe_list: "".b,
  sparse: [nil, nil],
  colors: []
)
edge_back = Codec.roundtrip_composite(edge)
expect(edge_back == edge, "edge Composite round-trips")
expect(edge_back.name == "" && edge_back.blob == "".b && edge_back.names == [] && edge_back.by_name == {},
       "empties survive")
expect(edge_back.some_text.nil? && edge_back.maybe_shape.nil? && edge_back.some_i64 == I64_MAX && edge_back.none_i64 == -1,
       "optionals in both states")
expect(edge_back.matrix == [[], [-(2**31), 0, 2**31 - 1]], "i32 extremes in a nested list (got #{edge_back.matrix})")
expect(edge_back.empty[1] == Float::INFINITY && edge_back.empty[2] == -Float::INFINITY, "infinities in a list")
expect(negative_zero?(edge_back.empty[3]) && !negative_zero?(edge_back.empty[4]), "-0.0 and 0.0 keep their signs")
expect(edge_back.maybe_list == "".b && !edge_back.maybe_list.nil?, "present empty optional bytes")
expect(edge_back.sparse == [nil, nil], "list of absent optionals")
expect(edge_back.by_id[0] == extremes, "record inside a map with an integer key")
expect(Codec.describe_composite(edge).start_with?("Composite {"), "describe_composite on the edge value")

# --- Shape (rich enum) ---------------------------------------------------
[
  Codec::Shape::Empty.new,
  Codec::Shape::Circle.new(radius: -0.0),
  Codec::Shape::Rect.new(width: 0.25, height: -Float::INFINITY),
  Codec::Shape::Labeled.new(label: "ünïcödé ✓", count: -(2**31)),
  Codec::Shape::Nested.new(inner: extremes, note: ""),
  Codec::Shape::Nested.new(inner: rebuilt, note: nil)
].each do |shape|
  back = Codec.roundtrip_shape(shape)
  expect(back == shape, "roundtrip_shape #{shape.class} (got #{Codec.describe_shape(back)})")
  expect(back.class == shape.class && back.tag == shape.tag, "roundtrip_shape keeps the variant")
end
expect(negative_zero?(Codec.roundtrip_shape(Codec::Shape::Circle.new(radius: -0.0)).radius), "-0.0 in a variant")
expect(Codec.describe_shape(Codec::Shape::Empty.new) == "Empty", "describe_shape Empty")
expect(Codec.describe_shape(Codec::Shape::Circle.new(radius: 2.5)) == "Circle { radius: 2.5 }", "describe_shape Circle")
expect(Codec.describe_shape(Codec::Shape::Labeled.new(label: "x", count: 1)) == 'Labeled { label: "x", count: 1 }',
       "describe_shape Labeled")
expect(Codec.roundtrip_shapes([]) == [], "empty list of shapes")
expect(Codec.roundtrip_shapes(composite.shapes) == composite.shapes, "list of every variant")
expect(Codec.roundtrip_shapes(composite.shapes).map(&:tag) == [0, 1, 2, 3, 4], "tags preserved in a list")

# --- Standalone shapes: optionals, maps, strings, bytes, scalars ---------
expect(Codec.roundtrip_opt_i64(nil).nil?, "opt i64 absent")
expect(Codec.roundtrip_opt_i64(0).zero?, "opt i64 zero is present")
expect(Codec.roundtrip_opt_i64(I64_MIN) == I64_MIN, "opt i64::MIN")
expect(Codec.roundtrip_opt_i64(I64_MAX) == I64_MAX, "opt i64::MAX")
expect(Codec.roundtrip_map({}) == {}, "empty map")
unicode_map = { "" => 0, "ключ" => I64_MIN, "✓" => I64_MAX, "a" => -1 }
expect(Codec.roundtrip_map(unicode_map) == unicode_map, "unicode-keyed map with extremes")
expect(Codec.roundtrip_string("") == "", "empty string")
expect(Codec.roundtrip_string("héllo ✓ 日本") == "héllo ✓ 日本", "unicode string")
expect(Codec.roundtrip_string("héllo").encoding == Encoding::UTF_8, "returned string is UTF-8")
expect(Codec.roundtrip_bytes("".b) == "".b, "empty bytes")
all_bytes = (0..255).to_a.pack("C*")
expect(Codec.roundtrip_bytes(all_bytes) == all_bytes, "every byte value, including NUL")
expect(Codec.roundtrip_bytes(all_bytes).encoding == Encoding::BINARY, "returned bytes are BINARY")
expect(Codec.roundtrip_i64(I64_MIN) == I64_MIN, "direct i64::MIN")
expect(Codec.roundtrip_i64(I64_MAX) == I64_MAX, "direct i64::MAX")
expect(Codec.roundtrip_i64(-1) == -1, "direct -1")
expect(Codec.roundtrip_u64(U64_MAX) == U64_MAX, "direct u64::MAX")
expect(Codec.roundtrip_u64(2**63) == 2**63, "direct 2^63 stays unsigned")
expect(Codec.roundtrip_u64(0).zero?, "direct u64 zero")
expect(Codec.roundtrip_f64(Float::NAN).nan?, "direct NaN")
expect(Codec.roundtrip_f64(Float::INFINITY) == Float::INFINITY, "direct +inf")
expect(Codec.roundtrip_f64(-Float::INFINITY) == -Float::INFINITY, "direct -inf")
expect(negative_zero?(Codec.roundtrip_f64(-0.0)), "direct -0.0 keeps its sign")
expect(Codec.roundtrip_f64(Float::MAX) == Float::MAX, "direct f64::MAX")
expect(Codec.roundtrip_f64(Float::MIN) == Float::MIN, "direct smallest normal")
expect(Codec.roundtrip_f64(5e-324) == 5e-324, "direct subnormal")
expect(Codec.roundtrip_bool(true) == true && Codec.roundtrip_bool(false) == false, "direct bool")
expect(Codec.roundtrip_color(Codec::Color::BLUE) == Codec::Color::BLUE, "direct enum BLUE")
expect(Codec.roundtrip_color(Codec::Color::RED) == Codec::Color::RED, "direct enum RED")

# --- Holder (objects inside buffers) ---------------------------------------
holder = Codec.make_holder(10, true)
expect(holder.is_a?(Codec::Holder), "make_holder returns a Holder")
expect(holder.primary.is_a?(Codec::Token) && !holder.primary.closed?, "primary is an open Token")
expect(holder.primary.value == 10, "primary value (got #{holder.primary.value})")
expect(holder.spare.is_a?(Codec::Token) && holder.spare.value == 11, "spare value")
expect(holder.many.length == 3 && holder.many.map(&:value) == [12, 13, 14], "many values (got #{holder.many.map(&:value)})")
addresses = ([holder.primary, holder.spare] + holder.many).map { |t| t.handle.address }
expect(addresses.uniq.length == 5, "five distinct token objects")

# Encoding a Holder mints one reference per token, which the producer consumes.
expect(Codec.sum_holder(holder) == 10 + 11 + 12 + 13 + 14, "sum_holder (got #{Codec.sum_holder(holder)})")
expect(Codec.sum_holder(holder) == 60, "sum_holder is repeatable (tokens not consumed)")
expect(holder.primary.value == 10 && holder.spare.value == 11, "tokens usable after being encoded")

# primary_of returns a wrapper to the SAME object as holder.primary.
primary = Codec.primary_of(holder)
expect(primary.is_a?(Codec::Token), "primary_of returns a Token")
expect(!primary.equal?(holder.primary), "primary_of returns a distinct wrapper")
expect(primary.handle.address == holder.primary.handle.address, "primary_of wraps the same object")
expect(primary.value == 10, "primary_of value")
primary.close
primary.close
expect(primary.closed?, "primary_of wrapper closed idempotently")
expect(holder.primary.value == 10, "original primary alive after closing the returned wrapper")

# same_primary compares object identity through two encoded buffers.
expect(Codec.same_primary(holder, holder) == true, "same_primary(h, h)")
other = Codec.make_holder(10, true)
expect(other.primary.value == 10, "other holder has equal values")
expect(Codec.same_primary(holder, other) == false, "same_primary is identity, not value")
aliased = Codec::Holder.new(primary: holder.primary, spare: nil, many: [])
expect(Codec.same_primary(holder, aliased) == true, "a Ruby-built Holder sharing the wrapper is the same primary")
twin = holder.primary.dup
expect(twin.handle.address == holder.primary.handle.address, "dup of a Token wraps the same object")
expect(Codec.same_primary(holder, Codec::Holder.new(primary: twin, spare: nil, many: [])) == true,
       "a dup'd wrapper is still the same object")
twin.close

# A Holder built entirely in Ruby from constructor-made tokens.
t1 = Codec::Token.new(1)
t2 = Codec::Token.new(2)
t3 = Codec::Token.new(-3)
expect(t1.value == 1 && t3.value == -3, "Token.new values")
built = Codec::Holder.new(primary: t1, spare: t2, many: [t3, t1, t1])
expect(Codec.sum_holder(built) == 1 + 2 + -3 + 1 + 1, "sum_holder over a Ruby-built Holder (repeated object)")
expect(Codec.primary_of(built).handle.address == t1.handle.address, "primary_of a Ruby-built Holder")
expect(Codec.sum_holder(Codec::Holder.new(primary: t1, spare: nil, many: [])) == 1, "Holder with nothing optional")
expect(Codec.sum_holder(Codec::Holder.new(primary: Codec::Token.new(I64_MAX), spare: nil, many: [])) == I64_MAX,
       "i64::MAX through a token")

# Absent optional object.
without = Codec.make_holder(0, false)
expect(without.spare.nil?, "make_holder(_, false) has no spare")
expect(without.primary.value.zero? && without.many.map(&:value) == [2, 3, 4], "values without spare")
expect(Codec.sum_holder(without) == 9, "sum without spare")

# A closed token inside a Holder is refused before anything is encoded.
t2.close
begin
  Codec.sum_holder(built)
  raise "expected Error encoding a closed Token"
rescue Codec::Error => e
  expect(e.message.include?("after close"), "closed token in a buffer (got #{e.message.inspect})")
end
begin
  t2.value
  raise "expected Error when using a closed Token"
rescue Codec::Error => e
  expect(e.message.include?("after close"), "use-after-close message")
end

# Release: every wrapper closes exactly once, double close is a no-op, and
# wrappers left to GC release without double frees.
[holder, other, without].each do |h|
  h.primary.close
  h.spare&.close
  h.many.each(&:close)
  h.primary.close
  h.many.each(&:close)
  expect(h.primary.closed? && h.many.all?(&:closed?), "holder tokens closed")
end
t1.close
t3.close
t1.close
200.times { |i| Codec.sum_holder(Codec.make_holder(i, i.odd?)) }
GC.start
GC.start
expect(Codec.roundtrip_i64(42) == 42, "library still healthy after GC of unclosed wrappers")

puts "ruby/codec: OK"
