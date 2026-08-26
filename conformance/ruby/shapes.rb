# frozen_string_literal: true
# Conformance consumer: shapes sample, Ruby target.
#
# Drives the generated rich (algebraic) enum surface: Shape is a sum type of
# plain value classes (Shape::Empty, Shape::Circle, Shape::Rectangle, and
# Shape::Labeled), each with keyword construction, field readers, an integer
# `tag` reader backed by per-variant TAG constants, and structural equality.
# The free functions encode the shape into a value buffer on the way in and
# decode the returned buffer into a fresh variant instance. Also covers the
# expanded numerics (f32 fields, u8 field, bytes in, u64 out). The cdylib is
# selected via WEAVEFFI_LIBRARY. Non-zero exit on any failed assertion.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "shapes"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# Unit variant: no fields, tag 0.
empty = Shapes::Shape::Empty.new
expect(empty.is_a?(Shapes::Shape), "Empty is a Shape")
expect(empty.tag == Shapes::Shape::Empty::TAG, "empty tag is the TAG constant (got #{empty.tag})")
expect(empty.tag.zero?, "empty tag == 0 (got #{empty.tag})")
expect(empty == Shapes::Shape::Empty.new, "unit variants compare structurally")

# f64 payload.
circle = Shapes::Shape::Circle.new(radius: 2.5)
expect(circle.tag == Shapes::Shape::Circle::TAG, "circle tag is the TAG constant (got #{circle.tag})")
expect(circle.tag == 1, "circle tag == 1 (got #{circle.tag})")
expect((circle.radius - 2.5).abs < 1e-9, "circle radius == 2.5 (got #{circle.radius})")

# Two f32 payloads.
rect = Shapes::Shape::Rectangle.new(width: 3.0, height: 4.0)
expect(rect.tag == 2, "rectangle tag == 2 (got #{rect.tag})")
expect((rect.width - 3.0).abs < 1e-6, "rectangle width == 3.0 (got #{rect.width})")
expect((rect.height - 4.0).abs < 1e-6, "rectangle height == 4.0 (got #{rect.height})")

# string + u8 payload.
labeled = Shapes::Shape::Labeled.new(label: "hex", count: 6)
expect(labeled.tag == 3, "labeled tag == 3 (got #{labeled.tag})")
expect(labeled.label == "hex", "labeled label == hex (got #{labeled.label.inspect})")
expect(labeled.count == 6, "labeled count == 6 (got #{labeled.count})")

# Free functions: Shape in (encoded into a value buffer), string/Shape out.
expect(Shapes.describe(circle) == "circle(r=2.5)", "describe(circle) (got #{Shapes.describe(circle).inspect})")
expect(Shapes.describe(rect).include?("rect"), "describe(rect) mentions rect")
expect(Shapes.describe(labeled).include?("hex"), "describe(labeled) mentions the label")

big = Shapes.scale(circle, 4.0)
expect(big.is_a?(Shapes::Shape::Circle), "scaled variant is Circle (got #{big.class})")
expect((big.radius - 10.0).abs < 1e-9, "scaled radius == 10.0 (got #{big.radius})")
expect(big == Shapes::Shape::Circle.new(radius: 10.0), "scaled shape compares structurally")

# A C-style enum keeps its plain integer constants.
expect(Shapes::Channel::GREEN == 1, "Channel::GREEN == 1")

# Numerics: `[u8]` canonicalizes to `bytes`, so the parameter is a binary
# string; the u64 sum comes back as a plain Integer.
raw = [250, 250, 250, 250].pack("C*")
expect(Shapes.sum_bytes(raw) == 1000, "sum_bytes == 1000 (got #{Shapes.sum_bytes(raw)})")

puts "ruby/shapes: OK"
