# frozen_string_literal: true
# Conformance consumer: contacts sample, Ruby target.
#
# Drives the 0.5.0 interface surface: ContactBook is a generated Ruby class
# wrapping the owned C object (released through FFI::AutoPointer), `new` maps
# to initialize, and methods pass the handle as the leading C argument.
# Throwing methods raise the typed ContactsError subclasses (InvalidName=1,
# NotFound=2); non-throwing methods keep the generic WeaveFFI::Error for
# panics only. The cdylib is selected via WEAVEFFI_LIBRARY.

$LOAD_PATH.unshift(File.join(ENV.fetch("WV_RB"), "lib"))
require "contacts"

def expect(cond, msg)
  raise "assertion failed: #{msg}" unless cond
end

book = WeaveFFI::ContactBook.new

alice = book.add("Alice", "Smith", "alice@example.com", WeaveFFI::ContactType::WORK)
expect(alice.id.positive?, "alice id positive")
expect(alice.first_name == "Alice", "first_name")
expect(alice.last_name == "Smith", "last_name")
expect(alice.email == "alice@example.com", "email")
expect(alice.contact_type == WeaveFFI::ContactType::WORK, "contact_type")

# Optional string: a missing email round-trips as nil.
bob = book.add("Bob", "Jones", nil, WeaveFFI::ContactType::PERSONAL)
expect(book.get(bob.id).email.nil?, "bob email nil")

expect(book.count == 2, "count == 2")
everyone = book.list
expect(everyone.length == 2, "list length == 2")
expect(everyone.map(&:first_name).sort == %w[Alice Bob], "list names")

expect(book.remove(alice.id) == true, "remove returns true")
expect(book.count == 1, "count == 1 after remove")
expect(book.remove(alice.id) == false, "second remove returns false")

# Typed errors: each domain code raises its own subclass of the domain
# class, which itself subclasses the generic error.
begin
  book.add("", "Smith", nil, WeaveFFI::ContactType::PERSONAL)
  raise "expected ContactsError::InvalidName for empty name"
rescue WeaveFFI::ContactsError::InvalidName => e
  expect(e.code == 1, "InvalidName code == 1 (got #{e.code})")
end

begin
  book.get(9999)
  raise "expected ContactsError::NotFound for missing contact"
rescue WeaveFFI::ContactsError::NotFound => e
  expect(e.code == 2, "NotFound code == 2 (got #{e.code})")
  expect(e.is_a?(WeaveFFI::ContactsError), "NotFound is a ContactsError")
  expect(e.is_a?(WeaveFFI::Error), "domain errors subclass WeaveFFI::Error")
end

# Rescuing the domain base class catches any code in the domain.
begin
  book.get(9999)
  raise "expected ContactsError"
rescue WeaveFFI::ContactsError => e
  expect(e.code == 2, "domain rescue sees NotFound code (got #{e.code})")
end

# Each book owns independent state; explicit destroy releases it early (the
# AutoPointer's GC release is then a no-op, so no double-free).
other = WeaveFFI::ContactBook.new
expect(other.count.zero?, "fresh book empty")
other.destroy

puts "ruby/contacts: OK"
