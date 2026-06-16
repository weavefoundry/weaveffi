# frozen_string_literal: true
# Conformance consumer: shapes sample, Ruby target.
#
# Drives the generated rich (algebraic) enum surface: the opaque-handle `Shape`
# class (FFI::AutoPointer cleanup), its integer `tag` reader plus per-variant
# tag constants, the per-variant factory class methods (`Shape.circle(2.5)`),
# and the variant-namespaced field accessors (`circle_radius`), plus the free
# functions that take and return `Shape`. Also covers the expanded numerics
# (f32 fields, u8 field, list<u8> in, u64 out). The cdylib is selected via
# WEAVEFFI_LIBRARY. Non-zero exit on any failed assertion.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "shapes"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

# Unit variant: no payload, tag 0.
empty = Shapes::Shape.empty
expect(empty.tag == Shapes::Shape::EMPTY, "empty tag is EMPTY constant (got #{empty.tag})")
expect(empty.tag.zero?, "empty tag == 0 (got #{empty.tag})")

# f64 payload.
circle = Shapes::Shape.circle(2.5)
expect(circle.tag == Shapes::Shape::CIRCLE, "circle tag is CIRCLE (got #{circle.tag})")
expect((circle.circle_radius - 2.5).abs < 1e-9, "circle radius == 2.5 (got #{circle.circle_radius})")

# Two f32 payloads.
rect = Shapes::Shape.rectangle(3.0, 4.0)
expect(rect.tag == Shapes::Shape::RECTANGLE, "rectangle tag is RECTANGLE (got #{rect.tag})")
expect((rect.rectangle_width - 3.0).abs < 1e-6, "rectangle width == 3.0 (got #{rect.rectangle_width})")
expect((rect.rectangle_height - 4.0).abs < 1e-6, "rectangle height == 4.0 (got #{rect.rectangle_height})")

# string + u8 payload.
labeled = Shapes::Shape.labeled("hex", 6)
expect(labeled.tag == Shapes::Shape::LABELED, "labeled tag is LABELED (got #{labeled.tag})")
expect(labeled.labeled_label == "hex", "labeled label == hex (got #{labeled.labeled_label.inspect})")
expect(labeled.labeled_count == 6, "labeled count == 6 (got #{labeled.labeled_count})")

# Free functions: Shape in, string/Shape out.
expect(Shapes.describe(circle) == "circle(r=2.5)", "describe(circle) (got #{Shapes.describe(circle).inspect})")

big = Shapes.scale(circle, 4.0)
expect(big.tag == Shapes::Shape::CIRCLE, "scaled tag is CIRCLE (got #{big.tag})")
expect((big.circle_radius - 10.0).abs < 1e-9, "scaled radius == 10.0 (got #{big.circle_radius})")

# Numerics: list<u8> in, u64 out.
expect(Shapes.sum_bytes([250, 250, 250, 250]) == 1000, "sum_bytes == 1000 (got #{Shapes.sum_bytes([250, 250, 250, 250])})")

# Shape handles wrap their pointer in FFI::AutoPointer and free on GC; calling
# the explicit destroy as well would double-free, so we rely on the GC.

puts "ruby/shapes: OK"
