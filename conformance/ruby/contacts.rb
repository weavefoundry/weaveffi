# frozen_string_literal: true
# Conformance consumer: contacts sample, Ruby target.
#
# Drives the value-type surface: Contact is a plain Ruby value class decoded
# from the value buffer the producer returns (the optional email is nil-able
# and the list return is a native Array), while ContactBook is a
# reference-counted interface wrapper whose `close` releases its strong
# reference early (idempotent; the AutoPointer otherwise releases it at GC
# time), and whose `dup` mints an independent wrapper. Throwing methods
# raise the typed ContactsError subclasses (InvalidName=1, NotFound=2);
# non-throwing methods keep the generic Contacts::Error for panics only. The
# nested contacts.groups module uses the parent's enum, interface, and error
# domain; the sibling directory root shares the Contact record. The gem is
# installed from the generated tree; the cdylib is selected through
# CONTACTS_LIBRARY.

require_relative 'support'
require 'contacts'

run_and_check_leaks(Contacts) do
  expect(Contacts::ABI_VERSION == 3, "bindings target ABI revision 3")
  book = Contacts::ContactBook.new

  alice = book.add("Alice", "Smith", "alice@example.com", Contacts::ContactType::WORK)
  expect(alice.is_a?(Contacts::Contact), "add returns a Contact value")
  expect(alice.id.positive?, "alice id positive")
  expect(alice.first_name == "Alice", "first_name")
  expect(alice.last_name == "Smith", "last_name")
  expect(alice.email == "alice@example.com", "email")
  expect(alice.contact_type == Contacts::ContactType::WORK, "contact_type")

  # get decodes a fresh snapshot; value classes compare structurally.
  expect(book.get(alice.id) == alice, "get snapshot equals added contact")

  # Optional string: a missing email round-trips as nil.
  bob = book.add("Bob", "Jones", nil, Contacts::ContactType::PERSONAL)
  expect(book.get(bob.id).email.nil?, "bob email nil")

  expect(book.count == 2, "count == 2")
  everyone = book.list
  expect(everyone.is_a?(Array), "list returns a native Array")
  expect(everyone.length == 2, "list length == 2")
  expect(everyone.map(&:first_name).sort == %w[Alice Bob], "list names")

  expect(book.remove(alice.id) == true, "remove returns true")
  expect(book.count == 1, "count == 1 after remove")
  expect(book.remove(alice.id) == false, "second remove returns false")

  # Typed errors: each domain code raises its own subclass of the domain
  # class, which itself subclasses the generic error.
  begin
    book.add("", "Smith", nil, Contacts::ContactType::PERSONAL)
    raise "expected ContactsError::InvalidName for empty name"
  rescue Contacts::ContactsError::InvalidName => e
    expect(e.code == 1, "InvalidName code == 1 (got #{e.code})")
  end

  begin
    book.get(9999)
    raise "expected ContactsError::NotFound for missing contact"
  rescue Contacts::ContactsError::NotFound => e
    expect(e.code == 2, "NotFound code == 2 (got #{e.code})")
    expect(e.is_a?(Contacts::ContactsError), "NotFound is a ContactsError")
    expect(e.is_a?(Contacts::Error), "domain errors subclass Contacts::Error")
  end

  # Rescuing the domain base class catches any code in the domain.
  begin
    book.get(9999)
    raise "expected ContactsError"
  rescue Contacts::ContactsError => e
    expect(e.code == 2, "domain rescue sees NotFound code (got #{e.code})")
  end

  # Each book owns independent state. `dup` produces a second wrapper holding
  # its own strong reference to the same object, so state is shared and closing
  # one wrapper leaves the other usable.
  other = Contacts::ContactBook.new
  expect(other.count.zero?, "fresh book empty")
  twin = other.dup
  expect(twin.handle.address == other.handle.address, "dup wraps the same object")
  other.add("Carol", "White", nil, Contacts::ContactType::PERSONAL)
  expect(twin.count == 1, "state visible through the dup (got #{twin.count})")
  twin.close
  expect(twin.closed?, "dup reports closed")
  expect(!other.closed?, "original still open after closing the dup")
  expect(other.count == 1, "original usable after closing the dup")

  # Explicit close releases the wrapper's reference early; a second close is a
  # no-op (the AutoPointer's GC release is also a no-op afterward, so no
  # double-free), and using a closed wrapper raises the brand error.
  other.close
  other.close
  expect(other.closed?, "closed? after close")
  begin
    other.count
    raise "expected Error when using a closed ContactBook"
  rescue Contacts::Error => e
    expect(e.message.include?("after close"), "use-after-close message (got #{e.message.inspect})")
  end
  book.close

  # contacts.groups is nested under the contacts root: its functions take and
  # return the parent's ContactType enum and ContactBook interface and report
  # the parent's ContactsError.
  team = Contacts::ContactBook.new
  team.add("Ada", "Lovelace", "ada@example.com", Contacts::ContactType::WORK)
  team.add("Grace", "Hopper", nil, Contacts::ContactType::WORK)
  team.add("Linus", "Pauling", nil, Contacts::ContactType::PERSONAL)
  expect(Contacts.count_of_type(team, Contacts::ContactType::WORK) == 2, "count_of_type WORK")
  expect(Contacts.count_of_type(team, Contacts::ContactType::OTHER).zero?, "count_of_type OTHER")
  expect(Contacts.dominant_type(team) == Contacts::ContactType::WORK, "dominant_type is WORK")
  work = Contacts.split_by_type(team, Contacts::ContactType::WORK)
  expect(work.is_a?(Contacts::ContactBook), "split_by_type returns a ContactBook")
  expect(work.handle.address != team.handle.address, "split_by_type returns a new book")
  expect(work.count == 2 && team.count == 3, "split copies only WORK contacts")
  first = Contacts.first_of_type(team, Contacts::ContactType::PERSONAL)
  expect(first.is_a?(Contacts::Contact) && first.first_name == "Linus", "first_of_type PERSONAL")
  begin
    Contacts.first_of_type(work, Contacts::ContactType::PERSONAL)
    raise "expected ContactsError::NotFound from the nested module"
  rescue Contacts::ContactsError::NotFound => e
    expect(e.code == 2, "nested module reports the parent's domain (got #{e.code})")
  end

  # directory is a sibling root sharing the Contact record (a value type) and
  # declaring its own DirectoryError domain.
  card = Contacts.card(first)
  expect(card == Contacts::Card.new(display_name: "Pauling, Linus", initials: "LP", has_email: false),
         "card (got #{card.display_name.inspect} #{card.initials.inspect} #{card.has_email})")
  expect(Contacts.card(team.list[0]).has_email, "card sees the email")
  sorted = Contacts.sorted(team.list)
  expect(sorted.map(&:last_name) == %w[Hopper Lovelace Pauling], "sorted by last name (got #{sorted.map(&:last_name)})")
  begin
    Contacts.sorted([])
    raise "expected DirectoryError::Empty"
  rescue Contacts::DirectoryError::Empty => e
    expect(e.code == 1, "DirectoryError::Empty code (got #{e.code})")
    expect(!e.is_a?(Contacts::ContactsError), "the sibling root's domain is distinct")
  end
  work.close
  team.close
end

puts "ruby/contacts: OK"
