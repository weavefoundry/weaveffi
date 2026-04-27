#!/usr/bin/env ruby
# frozen_string_literal: true

require "ffi"
require "weaveffi"

TYPE_LABELS = {
  WeaveFFI::ContactType::PERSONAL => "Personal",
  WeaveFFI::ContactType::WORK => "Work",
  WeaveFFI::ContactType::OTHER => "Other"
}.freeze

def print_contact(contact)
  email = contact.email.nil? ? "" : " <#{contact.email}>"
  label = TYPE_LABELS.fetch(contact.contact_type, "Unknown")
  puts "  [#{contact.id}] #{contact.first_name} #{contact.last_name}#{email} (#{label})"
end

def release_contacts(contacts)
  contacts.each(&:destroy)
end

def demonstrate_auto_pointer_cleanup(contact_id)
  contact = WeaveFFI.get_contact(contact_id)

  puts "\nAutoPointer cleanup:"
  puts "  handle class: #{contact.handle.class}"
  puts "  owned by FFI::AutoPointer: #{contact.handle.is_a?(FFI::AutoPointer)}"
  puts "  dropping the Ruby wrapper lets ContactPtr.release free the native copy"

  contact = nil
  GC.start
  puts "  GC.start completed"
end

puts "=== Ruby Contacts Example ==="
puts

alice_id = WeaveFFI.create_contact(
  "Alice",
  "Smith",
  "alice@example.com",
  WeaveFFI::ContactType::PERSONAL
)
puts "Created contact ##{alice_id}"

bob_id = WeaveFFI.create_contact(
  "Bob",
  "Jones",
  nil,
  WeaveFFI::ContactType::WORK
)
puts "Created contact ##{bob_id}"

puts "\nTotal: #{WeaveFFI.count_contacts} contacts\n\n"

contacts = WeaveFFI.list_contacts
puts "All contacts:"
contacts.each { |contact| print_contact(contact) }
release_contacts(contacts)
puts "Released list copies with Contact#destroy"

puts "\nGet contact ##{alice_id}:"
alice = WeaveFFI.get_contact(alice_id)
print_contact(alice)
alice.destroy

demonstrate_auto_pointer_cleanup(alice_id)

deleted = WeaveFFI.delete_contact(bob_id)
puts "\nDeleted contact ##{bob_id}: #{deleted}"
puts "Total: #{WeaveFFI.count_contacts} contacts"

remaining = WeaveFFI.list_contacts
puts "\nRemaining contacts:"
remaining.each { |contact| print_contact(contact) }
release_contacts(remaining)
