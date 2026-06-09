# frozen_string_literal: true
# Conformance consumer: contacts sample, Ruby target.
#
# Exercises the generated FFI module: enum constants, opaque-handle structs with
# getter methods, optional strings, list-of-struct returns, boolean returns, and
# the raised-exception error path. The cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "contacts"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

alice = WeaveFFI.create_contact("Alice", "Smith", "alice@example.com", WeaveFFI::ContactType::WORK)
expect(alice > 0, "alice handle positive")

c = WeaveFFI.get_contact(alice)
expect(c.first_name == "Alice", "first_name")
expect(c.last_name == "Smith", "last_name")
expect(c.email == "alice@example.com", "email")
expect(c.contact_type == WeaveFFI::ContactType::WORK, "contact_type")

# Optional string: a missing email round-trips as nil.
bob = WeaveFFI.create_contact("Bob", "Jones", nil, WeaveFFI::ContactType::PERSONAL)
cb = WeaveFFI.get_contact(bob)
expect(cb.email.nil?, "bob email nil (got #{cb.email.inspect})")

expect(WeaveFFI.count_contacts == 2, "count == 2")
everyone = WeaveFFI.list_contacts
expect(everyone.length == 2, "list length == 2")
expect(everyone.map(&:first_name).sort == %w[Alice Bob], "list names")

expect(WeaveFFI.delete_contact(alice) == true, "delete returns true")
expect(WeaveFFI.count_contacts == 1, "count == 1 after delete")

begin
  WeaveFFI.get_contact(9999)
  raise "expected WeaveFFI::Error for missing contact"
rescue WeaveFFI::Error => e
  expect(e.code != 0, "error code non-zero")
end

puts "ruby/contacts: OK"
