# Kvstore consumer smoke test (Ruby / FFI).
#
# Loads KVSTORE_LIB at runtime via the `ffi` gem and exercises the
# minimum lifecycle every language binding must support: open store,
# put a value, get it back, delete it, close the store. Prints "OK"
# and exits 0 on success; any assertion failure exits 1.

require 'ffi'

class WeaveffiError < FFI::Struct
  layout :code, :int32, :message, :pointer
end

module Kv
  extend FFI::Library
  ffi_lib ENV.fetch('KVSTORE_LIB') { abort 'KVSTORE_LIB not set' }

  attach_function :weaveffi_kv_open_store, [:string, :pointer], :pointer
  attach_function :weaveffi_kv_close_store, [:pointer, :pointer], :void
  attach_function :weaveffi_kv_put,
                  [:pointer, :string, :pointer, :size_t, :int32, :pointer, :pointer],
                  :bool
  attach_function :weaveffi_kv_get, [:pointer, :string, :pointer], :pointer
  attach_function :weaveffi_kv_Entry_get_value, [:pointer, :pointer], :pointer
  attach_function :weaveffi_kv_Entry_destroy, [:pointer], :void
  attach_function :weaveffi_kv_delete, [:pointer, :string, :pointer], :bool
  attach_function :weaveffi_free_bytes, [:pointer, :size_t], :void
end

def check(cond, msg)
  return if cond

  warn "assertion failed: #{msg}"
  exit 1
end

err = WeaveffiError.new
store = Kv.weaveffi_kv_open_store('/tmp/kvstore-ruby-smoke', err)
check(err[:code].zero?, 'open_store error')
check(!store.null?, 'open_store returned null')

err = WeaveffiError.new
buf = FFI::MemoryPointer.new(:uint8, 5)
buf.put_bytes(0, 'hello')
ok = Kv.weaveffi_kv_put(store, 'greeting', buf, 5, 1, FFI::Pointer::NULL, err)
check(err[:code].zero?, 'put error')
check(ok, 'put returned false')

err = WeaveffiError.new
entry = Kv.weaveffi_kv_get(store, 'greeting', err)
check(err[:code].zero?, 'get error')
check(!entry.null?, 'get returned null')

len_ptr = FFI::MemoryPointer.new(:size_t)
value_ptr = Kv.weaveffi_kv_Entry_get_value(entry, len_ptr)
n = len_ptr.read(:size_t)
check(n == 5, "value length mismatch (got #{n})")
got = value_ptr.read_bytes(n)
check(got == 'hello', "value mismatch (got #{got.inspect})")
Kv.weaveffi_free_bytes(value_ptr, n)
Kv.weaveffi_kv_Entry_destroy(entry)

err = WeaveffiError.new
deleted = Kv.weaveffi_kv_delete(store, 'greeting', err)
check(err[:code].zero?, 'delete error')
check(deleted, 'delete did not return true')

err = WeaveffiError.new
Kv.weaveffi_kv_close_store(store, err)
check(err[:code].zero?, 'close_store error')

puts 'OK'
