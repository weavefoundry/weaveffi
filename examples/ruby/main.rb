# End-to-end consumer test for the Ruby binding consumers.
#
# Loads the calculator and contacts cdylibs at runtime via the `ffi`
# gem and exercises a representative slice of the C ABI: add,
# create_contact, list_contacts, delete_contact. Prints "OK" and exits
# 0 on success; any assertion failure exits 1.

require 'ffi'

class WeaveffiError < FFI::Struct
  layout :code, :int32, :message, :pointer
end

module Calc
  extend FFI::Library
  ffi_lib ENV.fetch('WEAVEFFI_LIB') { abort 'WEAVEFFI_LIB not set' }
  attach_function :weaveffi_calculator_add, [:int32, :int32, :pointer], :int32
end

module Contacts
  extend FFI::Library
  ffi_lib ENV.fetch('CONTACTS_LIB') { abort 'CONTACTS_LIB not set' }
  attach_function :weaveffi_contacts_create_contact,
                  [:string, :string, :string, :int32, :pointer], :uint64
  attach_function :weaveffi_contacts_list_contacts, [:pointer, :pointer], :pointer
  attach_function :weaveffi_contacts_Contact_get_id, [:pointer], :int64
  attach_function :weaveffi_contacts_Contact_list_free, [:pointer, :size_t], :void
  attach_function :weaveffi_contacts_delete_contact, [:uint64, :pointer], :int32
  attach_function :weaveffi_contacts_count_contacts, [:pointer], :int32
end

def check(cond, msg)
  return if cond

  warn "assertion failed: #{msg}"
  exit 1
end

err = WeaveffiError.new
sum = Calc.weaveffi_calculator_add(2, 3, err)
check(err[:code].zero?, 'calculator_add error')
check(sum == 5, 'calculator_add(2,3) != 5')

err = WeaveffiError.new
h = Contacts.weaveffi_contacts_create_contact('Alice', 'Smith', 'alice@example.com', 0, err)
check(err[:code].zero?, 'create_contact error')
check(!h.zero?, 'create_contact returned 0')

err = WeaveffiError.new
len_ptr = FFI::MemoryPointer.new(:size_t)
items = Contacts.weaveffi_contacts_list_contacts(len_ptr, err)
n = len_ptr.read(:size_t)
check(err[:code].zero?, 'list_contacts error')
check(n == 1, "list_contacts length != 1 (got #{n})")
check(!items.null?, 'list_contacts null')

first_ptr = items.read_pointer
check(Contacts.weaveffi_contacts_Contact_get_id(first_ptr) == h, 'id mismatch')
Contacts.weaveffi_contacts_Contact_list_free(items, n)

err = WeaveffiError.new
deleted = Contacts.weaveffi_contacts_delete_contact(h, err)
check(err[:code].zero?, 'delete_contact error')
check(deleted == 1, 'delete_contact did not return 1')

err = WeaveffiError.new
check(Contacts.weaveffi_contacts_count_contacts(err).zero?, 'store not empty after cleanup')

puts 'OK'
