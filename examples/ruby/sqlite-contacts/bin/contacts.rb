#!/usr/bin/env ruby
# frozen_string_literal: true

require "ffi"
require "thread"
require "time"
require "weaveffi"

STATUS_LABELS = {
  WeaveFFI::Status::ACTIVE => "Active",
  WeaveFFI::Status::ARCHIVED => "Archived"
}.freeze

def await_async
  queue = Queue.new
  yield proc { |result, err| queue.push([result, err]) }

  result, err = queue.pop
  raise err if err

  result
end

def print_contact(prefix, contact)
  email = contact.email || "no email"
  created = Time.at(contact.created_at).utc.iso8601
  status = STATUS_LABELS.fetch(contact.status, "Unknown")

  puts "#{prefix}##{contact.id} #{contact.name} <#{email}> (#{status}, created #{created})"
end

def destroy_all(contacts)
  contacts.each(&:destroy)
end

puts "=== Ruby SQLite Contacts Example ==="
puts

owned_contacts = []

begin
  alice = await_async do |done|
    WeaveFFI.create_contact_async("Alice", "alice@example.com") do |result, err|
      done.call(result, err)
    end
  end
  owned_contacts << alice
  puts "Created ##{alice.id} #{alice.name}"

  bob = await_async do |done|
    WeaveFFI.create_contact_async("Bob", nil, &done)
  end
  owned_contacts << bob
  puts "Created ##{bob.id} #{bob.name}"

  updated = await_async do |done|
    WeaveFFI.update_contact_async(alice.id, "alice@new.com", &done)
  end
  puts "Updated Alice's email: #{updated}"

  found = await_async do |done|
    WeaveFFI.find_contact_async(alice.id, &done)
  end
  begin
    raise "expected to find contact ##{alice.id}" if found.nil?

    puts
    print_contact("Found ", found)
  ensure
    found&.destroy
  end

  total = await_async do |done|
    WeaveFFI.count_contacts_async(nil, &done)
  end
  active = await_async do |done|
    WeaveFFI.count_contacts_async(WeaveFFI::Status::ACTIVE, &done)
  end
  puts "\nTotals: all=#{total} active=#{active}"

  contacts = WeaveFFI.list_contacts(nil)
  puts "\nAll contacts from Enumerator (#{contacts.class}):"
  contacts.each do |contact|
    begin
      print_contact("  ", contact)
    ensure
      contact.destroy
    end
  end

  deleted = await_async do |done|
    WeaveFFI.delete_contact_async(bob.id, &done)
  end
  puts "\nDeleted Bob: #{deleted}"

  remaining = await_async do |done|
    WeaveFFI.count_contacts_async(nil, &done)
  end
  puts "Remaining: #{remaining}"
ensure
  destroy_all(owned_contacts)
end
