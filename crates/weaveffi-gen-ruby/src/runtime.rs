//! The fixed Ruby runtime the generated module carries: the library loader,
//! the error surface, the value-buffer reader/writer pair, the runtime ABI
//! attachments (`error_set`, `error_clear`, `error_free`, `free_string`,
//! `free_bytes`), guarded by the load-time ABI-revision check, and (when
//! the API declares callback interfaces) the implementation registry the
//! vtable trampolines resolve their `ctx` keys against.

use weaveffi_core::cabi::ABI_VERSION;

/// The exact `ffi_lib` loader block [`render_preamble`] emits in `generate`
/// mode, so the packager can swap it for a bundled-first variant.
pub(crate) const RUBY_LOADER_ORIGINAL: &str = r#"  # An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a
  # specific build artifact regardless of its file name or location.
  _wv_override = ENV['WEAVEFFI_LIBRARY']
  if _wv_override && !_wv_override.empty?
    ffi_lib _wv_override
  else
    case FFI::Platform::OS
    when /darwin/
      ffi_lib 'libweaveffi.dylib'
    when /mswin|mingw/
      ffi_lib 'weaveffi.dll'
    else
      ffi_lib 'libweaveffi.so'
    end
  end"#;

/// The packaged `ffi_lib` loader for `lib`: prefer the per-platform library
/// bundled under `lib/native/`, then `WEAVEFFI_LIBRARY`, then the system path.
pub(crate) fn ruby_loader_packaged(lib: &str) -> String {
    format!(
        r#"  # A bundled per-platform library ships inside this gem; prefer it so the gem
  # works with no external setup. WEAVEFFI_LIBRARY still overrides.
  _wv_override = ENV['WEAVEFFI_LIBRARY']
  if _wv_override && !_wv_override.empty?
    ffi_lib _wv_override
  else
    case FFI::Platform::OS
    when /darwin/
      _wv_name = 'lib{lib}.dylib'
    when /mswin|mingw/
      _wv_name = '{lib}.dll'
    else
      _wv_name = 'lib{lib}.so'
    end
    _wv_bundled = File.join(__dir__, 'native', _wv_name)
    ffi_lib(File.exist?(_wv_bundled) ? _wv_bundled : _wv_name)
  end"#
    )
}

/// Emit the fixed module preamble: the loader, the error struct and generic
/// `Error` class, the buffer runtime, the runtime ABI attachments, the
/// generic `check_error!` trap helper, and (when the API declares callback
/// interfaces) the implementation registry.
pub(crate) fn render_preamble(out: &mut String, module_name: &str, has_callback_interfaces: bool) {
    out.push_str(&format!(
        "# frozen_string_literal: true
# {module_name} Ruby FFI bindings (auto-generated)

require 'ffi'

module {module_name}
  extend FFI::Library

{RUBY_LOADER_ORIGINAL}

  # The ABI revision these bindings were generated against. Checked before any
  # other symbol is attached so a mismatched producer fails at require time
  # instead of misreading the error struct or a value buffer later.
  ABI_VERSION = {ABI_VERSION}
  begin
    attach_function :weaveffi_abi_version, [], :uint32
  rescue FFI::NotFoundError
    raise LoadError, 'the loaded WeaveFFI library predates ABI versioning ' \\
                     \"(these bindings expect ABI revision #{{ABI_VERSION}})\"
  end
  _wv_abi = weaveffi_abi_version
  unless _wv_abi == ABI_VERSION
    raise LoadError, \"WeaveFFI ABI mismatch: these bindings expect revision #{{ABI_VERSION}} \" \\
                     \"but the loaded library reports revision #{{_wv_abi}}\"
  end

  class ErrorStruct < FFI::Struct
    layout :code, :int32,
           :message, :pointer,
           :payload_ptr, :pointer,
           :payload_len, :size_t
  end

  # The runtime-reserved error codes. Positive codes belong to a module's
  # declared error domain; these negative codes are programming errors the
  # wrappers raise as a plain Error regardless of the function's `throws`.
  GENERIC_ERROR_CODE = -1
  PANIC_ERROR_CODE = -2
  MARSHAL_ERROR_CODE = -3
  # A callback-interface implementation raised; the message carries the
  # exception text.
  FOREIGN_ERROR_CODE = -4

  class Error < StandardError
    attr_reader :code

    def initialize(code, message)
      @code = code
      super(message)
    end
  end
"
    ));
    out.push_str(RUBY_BUFFER_RUNTIME);
    out.push_str(
        "
  attach_function :weaveffi_error_set, [:pointer, :int32, :string], :void
  attach_function :weaveffi_error_clear, [:pointer], :void
  attach_function :weaveffi_error_free, [:pointer], :void
  attach_function :weaveffi_free_string, [:pointer], :void
  attach_function :weaveffi_free_bytes, [:pointer, :size_t], :void

  def self.check_error!(err)
    return if err[:code].zero?
    code = err[:code]
    msg_ptr = err[:message]
    msg = msg_ptr.null? ? '' : msg_ptr.read_string.force_encoding(Encoding::UTF_8)
    weaveffi_error_clear(err.to_ptr)
    raise Error.new(code, msg)
  end
",
    );
    if has_callback_interfaces {
        out.push_str(RUBY_CALLBACK_REGISTRY);
    }
}

/// The handle table behind every callback-interface parameter: the Ruby
/// implementation object is stored under an incrementing Integer key, and
/// that key (widened to a pointer) is what the producer receives as `ctx`.
/// The producer never sees a Ruby object address, so the GC is free to move
/// or keep the implementation as it likes; the entry is deleted when the
/// producer calls the vtable's `free`.
const RUBY_CALLBACK_REGISTRY: &str = "
  # Live callback-interface implementations keyed by the Integer the producer
  # holds as `ctx`. Keys start at 1 so a context pointer is never NULL.
  @wv_cb_registry = {}
  @wv_cb_next_key = 0
  @wv_cb_mutex = Mutex.new

  # @api private
  # Registers `impl` and returns the `ctx` pointer to hand the producer.
  def self._wv_cb_register(impl)
    @wv_cb_mutex.synchronize do
      @wv_cb_next_key += 1
      @wv_cb_registry[@wv_cb_next_key] = impl
      FFI::Pointer.new(@wv_cb_next_key)
    end
  end

  # @api private
  # Resolves a trampoline's `ctx` back to the registered implementation.
  def self._wv_cb_lookup(ctx)
    impl = @wv_cb_mutex.synchronize { @wv_cb_registry[ctx.address] }
    raise Error.new(FOREIGN_ERROR_CODE, 'callback context is not registered') if impl.nil?
    impl
  end

  # @api private
  # Drops the registry entry when the producer releases its last reference.
  def self._wv_cb_free(ctx)
    @wv_cb_mutex.synchronize { @wv_cb_registry.delete(ctx.address) }
    nil
  end

  # @api private
  # Reports a callback implementation's exception through `out_err`. The
  # producer copies the message, so the Ruby String need not outlive the call.
  def self._wv_cb_fail(out_err, exception)
    message = exception.message.to_s
    message = exception.class.name if message.empty?
    weaveffi_error_set(out_err, FOREIGN_ERROR_CODE, message)
    nil
  end
";

/// The private Ruby runtime implementing the value-buffer wire format
/// (little-endian, packed, no alignment): a writer building a binary String
/// and a reader that raises `Error` on any malformed buffer (truncation, bad
/// flag bytes, invalid UTF-8, length prefixes past the end, zero object
/// tokens, trailing bytes).
const RUBY_BUFFER_RUNTIME: &str = r#"
  # @api private
  # Appends values in the WeaveFFI value-buffer wire format: little-endian,
  # packed, no alignment.
  class WvBufferWriter
    def initialize
      @buf = +''.b
    end

    # The encoded bytes as a binary String.
    def data
      @buf
    end

    def write_bool(v)
      @buf << (v ? "\x01".b : "\x00".b)
    end

    def write_flag(v)
      write_bool(v)
    end

    def write_i8(v)
      @buf << [v].pack('c')
    end

    def write_u8(v)
      @buf << [v].pack('C')
    end

    def write_i16(v)
      @buf << [v].pack('s<')
    end

    def write_u16(v)
      @buf << [v].pack('S<')
    end

    def write_i32(v)
      @buf << [v].pack('l<')
    end

    def write_u32(v)
      @buf << [v].pack('L<')
    end

    def write_len(v)
      write_u32(v)
    end

    def write_i64(v)
      @buf << [v].pack('q<')
    end

    def write_u64(v)
      @buf << [v].pack('Q<')
    end

    def write_f32(v)
      @buf << [v].pack('e')
    end

    def write_f64(v)
      @buf << [v].pack('E')
    end

    def write_string(v)
      b = v.to_s.encode(Encoding::UTF_8).b
      write_u32(b.bytesize)
      @buf << b
    end

    def write_bytes(v)
      b = v.to_s.b
      write_u32(b.bytesize)
      @buf << b
    end
  end

  # @api private
  # Reads values in the WeaveFFI value-buffer wire format, raising Error on
  # any malformed buffer.
  class WvBufferReader
    def initialize(data)
      @data = data.to_s.b
      @pos = 0
    end

    def take(n, what)
      raise Error.new(-1, "malformed value buffer: #{what}") if @pos + n > @data.bytesize
      s = @data.byteslice(@pos, n)
      @pos += n
      s
    end

    def read_bool
      b = take(1, 'bool').unpack1('C')
      raise Error.new(-1, 'malformed value buffer: bool byte out of range') if b > 1
      b == 1
    end

    def read_flag
      b = take(1, 'option flag').unpack1('C')
      raise Error.new(-1, 'malformed value buffer: option flag out of range') if b > 1
      b == 1
    end

    def read_i8
      take(1, 'i8').unpack1('c')
    end

    def read_u8
      take(1, 'u8').unpack1('C')
    end

    def read_i16
      take(2, 'i16').unpack1('s<')
    end

    def read_u16
      take(2, 'u16').unpack1('S<')
    end

    def read_i32
      take(4, 'i32').unpack1('l<')
    end

    def read_u32
      take(4, 'u32').unpack1('L<')
    end

    def read_len
      len = read_u32
      if len > @data.bytesize - @pos
        raise Error.new(-1, 'malformed value buffer: length prefix exceeds remaining bytes')
      end
      len
    end

    def read_i64
      take(8, 'i64').unpack1('q<')
    end

    def read_u64
      take(8, 'u64').unpack1('Q<')
    end

    def read_f32
      take(4, 'f32').unpack1('e')
    end

    def read_f64
      take(8, 'f64').unpack1('E')
    end

    def read_string
      s = take(read_len, 'string bytes').force_encoding(Encoding::UTF_8)
      raise Error.new(-1, 'malformed value buffer: string is not valid UTF-8') unless s.valid_encoding?
      s
    end

    def read_bytes
      take(read_len, 'byte buffer')
    end

    # An object token: a non-zero u64 carrying one strong reference the
    # caller adopts into a wrapper.
    def read_object_token
      addr = read_u64
      raise Error.new(-1, 'malformed value buffer: null object token') if addr.zero?
      FFI::Pointer.new(addr)
    end

    def expect_end!
      return if @pos == @data.bytesize
      raise Error.new(-1, 'malformed value buffer: trailing bytes after value')
    end
  end
"#;
