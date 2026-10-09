# frozen_string_literal: true

require 'ffi'
require 'monitor'

# The fixed runtime every generated {{MODULE}} binding builds on. Its public
# part is the error hierarchy, CancelToken, and ABI_VERSION; everything else
# lives in two private modules: Native (the C library, attached through the
# ffi gem) and Bridge (the marshalling the generated wrappers share).
module {{MODULE}}
  # The C ABI revision these bindings speak. Loading fails unless the
  # library reports the same revision.
  ABI_VERSION = {{ABI_VERSION}}

  # Raised by `require` when the native library can't be loaded, reports
  # another ABI revision, or lacks a declaration these bindings were
  # generated with (or has one whose signature changed). It's a LoadError,
  # so a `rescue LoadError` around the require catches it too.
  class LoadError < ::LoadError; end

  # The base class of every error the library raises. A call that declares
  # `throws` raises its domain's error classes (subclasses of this one) for
  # domain codes, and this class itself for runtime codes (negative, below).
  class Error < StandardError
    # A generic failure, and every failure of a `throws: any` call.
    GENERIC = -1
    # The library panicked.
    PANIC = -2
    # An argument or a returned value couldn't be marshalled.
    MARSHAL = -3
    # A callback implementation failed and the call couldn't report it any
    # other way.
    CALLBACK = -4
    # The call was cancelled through its CancelToken (see Cancelled).
    CANCELLED = -5

    # The ABI error code: positive for a domain code, negative for a runtime
    # failure.
    # @return [Integer]
    attr_reader :code

    # @param message [String, nil] the message; an error code's class
    #   defaults to its documented message
    # @param code [Integer, nil] the code, for a class without a fixed
    #   `CODE` (an error code's class has one)
    # @param fields the payload fields of an error code that declares any,
    #   each required
    def initialize(message = nil, code: nil, **fields)
      code_class = Bridge.code_class(self.class)
      if code_class.nil?
        raise ArgumentError, "unknown keywords: #{fields.keys.join(', ')}" unless fields.empty?

        @code = code || GENERIC
      else
        unless code.nil? || code == code_class::CODE
          raise ArgumentError, "#{self.class} has the fixed code #{code_class::CODE}"
        end

        Bridge.assign_fields(self, code_class, fields)
        @code = code_class::CODE
        message ||= code_class::MESSAGE
      end
      super(message)
    end
  end

  # Raised when a call that declares no errors fails anyway. That's a bug in
  # the library (a panic, an argument it couldn't accept, a malformed value
  # it returned, a callback failure it let through), never an outcome to
  # handle; the message names the code and the library's message.
  class NativeBugError < Error
    # @param message [String, nil] the library's message
    # @param code [Integer] the runtime code
    def initialize(message = nil, code: MARSHAL)
      super("native call failed with code #{code}: #{message}", code: code)
    end
  end

  # Raised when a cancellable call completes because its CancelToken fired.
  class Cancelled < Error
    # @param message [String, nil] the library's message
    def initialize(message = nil)
      super(message || 'cancelled', code: CANCELLED)
    end
  end

  # @api private
  # The native library and its runtime functions. The generated bindings
  # attach every function they call here as well.
  module Native
    extend FFI::Library

    # An explicit path in {{LIBRARY_ENV}} wins. Otherwise a library bundled
    # under lib/native/ (a platform gem) is preferred over the system
    # search path.
    path = ENV.fetch('{{LIBRARY_ENV}}', '')
    if path.empty?
      name =
        case FFI::Platform::OS
        when /darwin/ then '{{LIB_MACOS}}'
        when /mswin|mingw|windows/ then '{{LIB_WINDOWS}}'
        else '{{LIB_LINUX}}'
        end
      bundled = File.expand_path(File.join('..', 'native', name), __dir__)
      path = File.exist?(bundled) ? bundled : name
    end
    begin
      ffi_lib(path)
    rescue ::LoadError => e
      raise {{MODULE}}::LoadError, "could not load the {{LIBRARY}} library (#{e.message})"
    end

    # Checked before anything else is attached, so a mismatched library
    # fails at require time instead of misreading an error struct or a value
    # buffer later.
    begin
      attach_function :{{PREFIX}}_abi_version, [], :uint32
    rescue FFI::NotFoundError
      raise {{MODULE}}::LoadError,
            "the loaded {{LIBRARY}} library does not export {{PREFIX}}_abi_version " \
            "(these bindings expect ABI revision #{ABI_VERSION})"
    end
    revision = {{PREFIX}}_abi_version
    unless revision == ABI_VERSION
      raise {{MODULE}}::LoadError,
            "{{LIBRARY}} ABI mismatch: these bindings expect revision #{ABI_VERSION} " \
            "but the loaded library reports revision #{revision}"
    end

    # The `{{PREFIX}}_error` out-parameter every call reports through. The
    # message is UTF-8, not NUL-terminated, and NULL when empty.
    class ErrorStruct < FFI::Struct
      layout :code, :int32,
             :message_ptr, :pointer,
             :message_len, :size_t,
             :payload_ptr, :pointer,
             :payload_len, :size_t
    end

    # One entry of a module's contract table.
    class ContractEntry < FFI::Struct
      layout :id, :uint64,
             :hash, :uint64
    end

    attach_function :{{PREFIX}}_error_set, [:pointer, :int32, :pointer, :size_t], :void
    attach_function :{{PREFIX}}_error_set_payload, [:pointer, :pointer, :size_t], :void
    attach_function :{{PREFIX}}_error_clear, [:pointer], :void
    attach_function :{{PREFIX}}_error_free, [:pointer], :void
    attach_function :{{PREFIX}}_alloc, [:size_t], :pointer
    attach_function :{{PREFIX}}_free_bytes, [:pointer, :size_t], :void
    attach_function :{{PREFIX}}_cancel_token_create, [], :pointer
    attach_function :{{PREFIX}}_cancel_token_cancel, [:pointer], :void
    attach_function :{{PREFIX}}_cancel_token_is_cancelled, [:pointer], :bool
    attach_function :{{PREFIX}}_cancel_token_destroy, [:pointer], :void
  end
  private_constant :Native

  # A cancellation signal for cancellable async calls. Pass one as the
  # `cancel:` keyword, then call #cancel from any thread; the call raises
  # Cancelled unless it already completed. A token may be shared by several
  # calls and stays cancelled once cancelled.
  class CancelToken
    # Owns the consumer's reference to the native token.
    class Ref < FFI::AutoPointer
      # @api private
      def self.release(ptr)
        Native.{{PREFIX}}_cancel_token_destroy(ptr)
      end
    end
    private_constant :Ref

    def initialize
      @ref = Ref.new(Native.{{PREFIX}}_cancel_token_create)
    end

    # Requests cancellation. Idempotent.
    # @return [nil]
    def cancel
      Native.{{PREFIX}}_cancel_token_cancel(@ref) unless @ref.nil?
      nil
    end

    # Whether #cancel has been called.
    # @return [Boolean]
    def cancelled?
      !@ref.nil? && Native.{{PREFIX}}_cancel_token_is_cancelled(@ref)
    end

    # Releases the native token now rather than at GC time. Calls already
    # launched with it keep their own reference.
    # @return [nil]
    def close
      @ref&.free
      @ref = nil
    end

    private

    def _wv_ptr
      raise Error, 'CancelToken used after close' if @ref.nil?

      @ref
    end
  end

  # @api private
  # The marshalling every generated wrapper shares: argument checks and
  # conversions, the call helpers (errors, out slots, returns), async
  # completions, iterators, interface wrappers, value buffers, and callback
  # interfaces.
  #
  # Values are described by small Ruby literals. A *kind* says how a return,
  # an async result, or an iterator element crosses: nil (void), a scalar
  # (`:i32`, `:f64`, `:bool`; a C-style enum is `:i32`), `:string`, `:bytes`,
  # `[:buffer, type]`, `[:slice, :i32]`, `[:opt, :i32]` (an optional scalar
  # crossing as a flag and a value), `[:object, Class]`, `[:object?, Class]`,
  # or `:pointer` (a constructor's raw pointer). A *type* says how a value
  # is written into a value buffer: a scalar, `:string`, `:bytes`,
  # `[:opt, type]`, `[:list, type]`, `[:map, key, value]`, or a class (a
  # record, a rich enum's module, or an interface).
  module Bridge
    # The ffi gem's name for each scalar.
    FFI_TYPES = {
      bool: :bool, i8: :int8, u8: :uint8, i16: :int16, u16: :uint16, i32: :int32,
      u32: :uint32, i64: :int64, u64: :uint64, f32: :float, f64: :double
    }.freeze

    # The inclusive range of each integer scalar.
    RANGES = {
      i8: -0x80..0x7f, u8: 0..0xff, i16: -0x8000..0x7fff, u16: 0..0xffff,
      i32: -0x8000_0000..0x7fff_ffff, u32: 0..0xffff_ffff,
      i64: -0x8000_0000_0000_0000..0x7fff_ffff_ffff_ffff, u64: 0..0xffff_ffff_ffff_ffff
    }.freeze

    # The size in bytes of each typed-array element.
    SIZES = { i8: 1, i16: 2, u16: 2, i32: 4, u32: 4, i64: 8, u64: 8, f32: 4, f64: 8 }.freeze

    # --- Scalars, strings, and typed arrays ---

    # `value` checked for scalar `kind`: an Integer in range (RangeError
    # rather than wrapping), a Numeric for a float, any truthiness for a
    # bool.
    def self.scalar(value, kind)
      case kind
      when :bool then value ? true : false
      when :f32, :f64
        raise TypeError, "expected a Float, got #{value.class}" unless value.is_a?(Numeric)

        value.to_f
      else
        raise TypeError, "expected an Integer for #{kind}, got #{value.class}" unless value.is_a?(Integer)
        raise RangeError, "#{value} is out of range for #{kind}" unless RANGES.fetch(kind).cover?(value)

        value
      end
    end

    # The `[has, value]` slots of an optional scalar argument.
    def self.opt_arg(value, kind)
      value.nil? ? [false, kind == :bool ? false : 0] : [true, scalar(value, kind)]
    end

    # The UTF-8 bytes of a string argument, as a private binary copy the
    # library borrows for the call.
    def self.string_arg(value)
      str = String.try_convert(value)
      raise TypeError, "expected a String, got #{value.class}" if str.nil?

      bytes =
        if str.encoding == Encoding::UTF_8 || str.encoding == Encoding::BINARY || str.ascii_only?
          str.b
        else
          str.encode(Encoding::UTF_8).b
        end
      raise ArgumentError, 'string is not valid UTF-8' unless bytes.force_encoding(Encoding::UTF_8).valid_encoding?

      bytes.force_encoding(Encoding::BINARY)
    rescue EncodingError => e
      raise ArgumentError, "string can't be converted to UTF-8 (#{e.message})"
    end

    # A private binary copy of a bytes argument.
    def self.bytes_arg(value)
      str = String.try_convert(value)
      raise TypeError, "expected a String of bytes, got #{value.class}" if str.nil?

      str.b
    end

    # The `[pointer, count]` slots of a typed-array argument: the elements,
    # range-checked, in an aligned native array (NULL when empty).
    def self.slice_arg(values, kind)
      ary = Array.try_convert(values)
      raise TypeError, "expected an Array, got #{values.class}" if ary.nil?
      return [nil, 0] if ary.empty?

      ptr = FFI::MemoryPointer.new(FFI_TYPES.fetch(kind), ary.length)
      ptr.public_send(:"put_array_of_#{FFI_TYPES.fetch(kind)}", 0, ary.map { |v| scalar(v, kind) })
      [ptr, ary.length]
    end

    # Copies a borrowed (ptr, len) run without releasing it.
    def self.borrow_bytes(ptr, len)
      ptr.null? || len.zero? ? ''.b : ptr.read_bytes(len)
    end

    # Copies a borrowed (ptr, len) UTF-8 run without releasing it.
    def self.borrow_string(ptr, len)
      borrow_bytes(ptr, len).force_encoding(Encoding::UTF_8)
    end

    # Copies a borrowed typed array of `len` elements without releasing it.
    def self.borrow_slice(ptr, len, kind)
      ptr.null? || len.zero? ? [] : ptr.public_send(:"get_array_of_#{FFI_TYPES.fetch(kind)}", 0, len)
    end

    # Copies a returned (ptr, len) run, then releases it.
    def self.take_bytes(ptr, len)
      data = borrow_bytes(ptr, len)
      Native.{{PREFIX}}_free_bytes(ptr, len) unless ptr.null?
      data
    end

    # Copies a returned UTF-8 run, then releases it.
    def self.take_string(ptr, len)
      take_bytes(ptr, len).force_encoding(Encoding::UTF_8)
    end

    # Copies a returned typed array, then releases it.
    def self.take_slice(ptr, len, kind)
      values = borrow_slice(ptr, len, kind)
      Native.{{PREFIX}}_free_bytes(ptr, len * SIZES.fetch(kind)) unless ptr.null?
      values
    end

    # --- Errors ---

    # Copies the code, message, and payload out of a reported error and
    # clears it, or returns nil when it reports success.
    def self.take_error(err)
      code = err[:code]
      return nil if code.zero?

      message = borrow_string(err[:message_ptr], err[:message_len])
      payload = err[:payload_ptr].null? ? nil : borrow_bytes(err[:payload_ptr], err[:payload_len])
      Native.{{PREFIX}}_error_clear(err.to_ptr)
      [code, message.empty? ? nil : message, payload]
    end

    # Raises the error a non-zero error slot reports, per `error`: nil for a
    # call that declares no errors (NativeBugError), Error for `throws:
    # any`, or a domain class.
    def self.check!(err, error)
      taken = take_error(err)
      raise failure(error, *taken) unless taken.nil?
    end

    # The exception for a reported failure. A positive code of a domain is
    # its code class (fields decoded from the payload), or the domain class
    # itself for a code these bindings don't know.
    def self.failure(error, code, message, payload)
      return Cancelled.new(message) if code == Error::CANCELLED
      return NativeBugError.new(message, code: code) if error.nil?
      return Error.new(message, code: code) unless code.positive? && error != Error

      code_class = codes(error)[code]
      return error.new(message, code: code) if code_class.nil?

      fields = RECORDS.key?(code_class) ? decode_bytes(payload.to_s, code_class, fields: true) : {}
      code_class.new(message, **fields)
    rescue NativeBugError => e
      e
    end

    # The error-code classes of a domain, by code: the classes nested in it
    # that carry a `CODE`.
    def self.codes(domain)
      @codes[domain] ||= domain.constants(false).filter_map do |name|
        cls = domain.const_get(name, false)
        [cls::CODE, cls] if cls.is_a?(Class) && cls < domain && cls.const_defined?(:CODE, false)
      end.to_h
    end
    @codes = {}

    # The class whose `CODE` an error class carries, or nil.
    def self.code_class(cls)
      cls.ancestors.find { |a| a.is_a?(Class) && a <= Error && a.const_defined?(:CODE, false) }
    end

    # Sets an error code's payload fields from `fields`, each required.
    def self.assign_fields(error, code_class, fields)
      names = RECORDS.fetch(code_class, {}).keys
      missing = names - fields.keys
      raise ArgumentError, "missing keywords: #{missing.join(', ')}" unless missing.empty?

      unknown = fields.keys - names
      raise ArgumentError, "unknown keywords: #{unknown.join(', ')}" unless unknown.empty?

      fields.each { |name, value| error.instance_variable_set(:"@#{name}", value) }
    end

    # --- Calls ---

    # Makes one synchronous call. Seals `writers` (the call's value-buffer
    # arguments), allocates the out slots `kind` needs, yields the sealed
    # encodings, the out slots, and the error slot, raises the reported
    # error, and returns the result received per `kind`.
    def self.call(error, kind = nil, *writers)
      err = Native::ErrorStruct.new
      outs = return_slots(kind)
      result = yield(*seal(*writers), *outs, err)
      check!(err, error)
      take_return(kind, result, outs)
    end

    # The out slots a synchronous return of `kind` needs.
    def self.return_slots(kind)
      case kind
      when :string, :bytes then [FFI::MemoryPointer.new(:size_t)]
      when Array
        case kind[0]
        when :buffer, :slice then [FFI::MemoryPointer.new(:size_t)]
        when :opt then [FFI::MemoryPointer.new(FFI_TYPES.fetch(kind[1]))]
        else []
        end
      else []
      end
    end

    # A synchronous call's result, received per `kind`.
    def self.take_return(kind, result, outs)
      case kind
      when :pointer then nonnull(result)
      when :string, :bytes then take_value(kind, result, outs[0].read(:size_t))
      when Array
        case kind[0]
        when :buffer, :slice then take_value(kind, result, outs[0].read(:size_t))
        when :opt then result ? outs[0].read(FFI_TYPES.fetch(kind[1])) : nil
        else take_value(kind, result)
        end
      else result
      end
    end

    # A value the library handed over (an async result or an iterator
    # element) from its slot values, received per `kind`: runs are copied
    # and released, objects adopted.
    def self.take_value(kind, *slots)
      case kind
      when nil then nil
      when :string then take_string(*slots)
      when :bytes then take_bytes(*slots)
      when Symbol then slots[0]
      else
        case kind[0]
        when :buffer then decode(slots[0], slots[1], kind[1])
        when :slice then take_slice(slots[0], slots[1], kind[1])
        when :opt then slots[0] ? slots[1] : nil
        when :object then adopt(kind[1], nonnull(slots[0]))
        when :object? then slots[0].null? ? nil : adopt(kind[1], slots[0])
        end
      end
    end

    # The C types of the slots a value of `kind` occupies as an async
    # result.
    def self.slot_types(kind)
      case kind
      when nil then []
      when :string, :bytes then %i[pointer size_t]
      when Symbol then [FFI_TYPES.fetch(kind)]
      else
        case kind[0]
        when :buffer, :slice then %i[pointer size_t]
        when :opt then [:bool, FFI_TYPES.fetch(kind[1])]
        else [:pointer]
        end
      end
    end

    # A returned object pointer that must not be NULL.
    def self.nonnull(ptr)
      raise NativeBugError.new('null object pointer', code: Error::MARSHAL) if ptr.null?

      ptr
    end

    # --- Async calls ---

    @completions = {}
    @completions_lock = Mutex.new

    # The completion function for async results of `kind`, one per result
    # slot signature for the life of the process (the library may call it
    # from any thread). It finds the pending call by `context`, receives the
    # error or the result, and delivers it to the waiting caller; nothing
    # raised while receiving escapes into the C frame.
    def self.completion(kind)
      types = slot_types(kind)
      @completions_lock.synchronize do
        @completions[types] ||= FFI::Function.new(:void, [:pointer, :pointer, *types]) do |context, err, *slots|
          entry = HANDLES.take(context)
          next if entry.nil?

          _, queue, error, result_kind = entry
          begin
            taken = take_boxed_error(err)
            queue << (taken.nil? ? take_value(result_kind, *slots) : failure(error, *taken))
          rescue Exception => e # rubocop:disable Lint/RescueException
            queue << e
          end
        end
      end
    end

    # Like take_error, for the heap-boxed error a completion receives; the
    # box is released.
    def self.take_boxed_error(err)
      return nil if err.null?

      taken = take_error(Native::ErrorStruct.new(err))
      Native.{{PREFIX}}_error_free(err)
      taken
    end

    # Launches an async call and blocks until it completes: yields the
    # sealed `writers`, the completion function, and the completion context
    # to the launch, then waits for the result or raises the error.
    def self.await(error, kind, *writers)
      pending(error, kind, writers) { |sealed, callback, context| yield(*sealed, callback, context) }
    end

    # Like await for a cancellable call: yields the native token (the
    # caller's `cancel`, or a private one) before the completion, so an
    # interrupted wait (Thread#raise, Timeout) cancels the library's work
    # too.
    def self.await_cancellable(error, kind, cancel, *writers)
      raise TypeError, "cancel must be a CancelToken, got #{cancel.class}" unless cancel.nil? || cancel.is_a?(CancelToken)

      own = CancelToken.new if cancel.nil?
      token = cancel || own
      pending(error, kind, writers, token) do |sealed, callback, context|
        yield(*sealed, token.__send__(:_wv_ptr), callback, context)
      end
    ensure
      own&.close
    end

    # Registers a pending call, launches it, and waits on its queue.
    def self.pending(error, kind, writers, token = nil)
      queue = Thread::Queue.new
      context = HANDLES.put([:async, queue, error, kind])
      launched = false
      begin
        yield seal(*writers), completion(kind), context
        launched = true
      ensure
        HANDLES.take(context) unless launched
      end
      wait(queue, token)
    end

    # Blocks until a pending call completes (Thread::Queue#pop, which
    # releases the GVL and, in a non-blocking Fiber under a Fiber scheduler,
    # yields to the scheduler), raising its error. If the wait is
    # interrupted, the call's token is cancelled before the interruption
    # propagates.
    def self.wait(queue, token)
      value =
        begin
          queue.pop
        rescue Exception # rubocop:disable Lint/RescueException
          token&.cancel
          raise
        end
      raise value if value.is_a?(Exception)

      value
    end

    # --- Iterators ---

    # A lazy Enumerator over a native iterator. Each enumeration launches
    # its own iterator (on the first pull, through the block, which receives
    # the sealed `writers` and the error slot), pulls one element per step
    # with `next_fn`, and releases the iterator with `destroy_fn` exactly
    # once: when iteration finishes, raises, or stops early, or (as a
    # backstop) from the GC finalizer of its FFI::AutoPointer, since Ruby
    # never runs the `ensure` of an external enumeration (`next`) that's
    # abandoned midway.
    def self.iterate(error, kind, next_fn, destroy_fn, *writers, &launch)
      Enumerator.new do |y|
        err = Native::ErrorStruct.new
        raw = launch.call(*seal(*writers), err)
        iter = FFI::AutoPointer.new(raw, Native.method(destroy_fn)) unless raw.null?
        begin
          check!(err, error)
          outs = item_slots(kind)
          advance = Native.method(next_fn)
          loop do
            more = advance.call(iter, *outs, err)
            check!(err, error)
            break if more.zero?

            y << take_value(kind, *read_slots(kind, outs))
          end
        ensure
          iter&.free
        end
      end
    end

    # The out slots of one iterator element of `kind`: one pointer to each
    # slot the element would occupy as an async result.
    def self.item_slots(kind)
      slot_types(kind).map { |type| FFI::MemoryPointer.new(type) }
    end

    # The values `_next` wrote to an element's out slots.
    def self.read_slots(kind, outs)
      slot_types(kind).zip(outs).map { |type, out| out.read(type) }
    end

    # --- Interfaces ---

    # The native lifecycle of each interface class: [clone, destroy].
    INTERFACES = {}

    # Registers the C functions that clone and release `cls`'s objects.
    def self.interface(cls, clone, destroy)
      INTERFACES[cls] = [Native.method(clone), Native.method(destroy)]
    end

    # The base of every interface wrapper. A wrapper owns one strong
    # reference to its native object and releases it exactly once, from
    # #close or (as a backstop) from the GC finalizer of its
    # FFI::AutoPointer. Calls borrow the pointer through Bridge.pin, so a
    # #close that races an in-flight call on another thread only releases
    # the reference once the call returns.
    class Handle
      # Whether #close has released this wrapper's reference.
      # @return [Boolean]
      def closed?
        @_wv_ref.nil? || @_wv_closing
      end

      # Releases this wrapper's reference now rather than at GC time.
      # Idempotent. The object itself lives until its last reference
      # anywhere (another wrapper, a record field, the library) goes.
      # @return [nil]
      def close
        ref = @_wv_lock.synchronize do
          next nil if closed?
          next _wv_detach if @_wv_calls.zero?

          @_wv_closing = true
          nil
        end
        ref&.free
        nil
      end

      # Two wrappers are equal when they're open and refer to the same
      # native object.
      # @return [Boolean]
      def ==(other)
        equal?(other) ||
          (other.instance_of?(self.class) && !closed? && !other.closed? && other._wv_address == _wv_address)
      end
      alias eql? ==

      # @return [Integer]
      def hash
        [self.class, _wv_address].hash
      end

      # @return [String]
      def inspect
        format('#<%<cls>s 0x%<address>016x%<state>s>', cls: self.class.name, address: _wv_address,
                                                        state: closed? ? ' (closed)' : '')
      end

      # dup and clone produce an independent wrapper with its own reference
      # to the same object.
      def initialize_copy(other)
        super
        _wv_init(Bridge.clone_ref(other))
      end

      protected

      attr_reader :_wv_address

      private

      def _wv_init(ptr)
        @_wv_ref = FFI::AutoPointer.new(ptr, INTERFACES.fetch(self.class)[1])
        @_wv_address = ptr.address
        @_wv_lock = Mutex.new
        @_wv_calls = 0
        @_wv_closing = false
      end

      # Yields the native pointer, keeping the reference alive until the
      # block returns.
      def _wv_call
        ref = @_wv_lock.synchronize do
          raise Error, "#{self.class.name} used after close" if closed?

          @_wv_calls += 1
          @_wv_ref
        end
        begin
          yield ref
        ensure
          _wv_leave
        end
      end

      def _wv_leave
        ref = @_wv_lock.synchronize do
          @_wv_calls -= 1
          next nil unless @_wv_closing && @_wv_calls.zero?

          @_wv_closing = false
          _wv_detach
        end
        ref&.free
      end

      def _wv_detach
        ref = @_wv_ref
        @_wv_ref = nil
        ref
      end
    end

    # Adopts one strong reference the library handed over into a new
    # `cls` wrapper.
    def self.adopt(cls, ptr)
      cls.allocate.tap { |obj| obj.__send__(:_wv_init, ptr) }
    end

    # Mints a new strong reference to `obj`'s native object, which the
    # caller owns.
    def self.clone_ref(obj)
      obj.__send__(:_wv_call) { |ptr| INTERFACES.fetch(obj.class)[0].call(ptr) }
    end

    # Raises TypeError unless `value` is a `cls` wrapper (or nil, when
    # `nullable`), so an object of the wrong interface never reaches the
    # library.
    def self.check_object(value, cls, name, nullable: false)
      return if value.is_a?(cls) || (nullable && value.nil?)

      expected = nullable ? "a #{cls.name} or nil" : "a #{cls.name}"
      raise TypeError, "#{name} must be #{expected}, got #{value.class}"
    end

    # Yields the native pointers of `objects` (NULL for nil), each kept
    # alive until the block returns.
    def self.pin(*objects, &block)
      pin_from(objects, 0, [], &block)
    end

    def self.pin_from(objects, index, pointers, &block)
      return yield(*pointers) if index == objects.length

      object = objects[index]
      return pin_from(objects, index + 1, pointers + [nil], &block) if object.nil?

      object.__send__(:_wv_call) { |ptr| pin_from(objects, index + 1, pointers + [ptr], &block) }
    end

    # --- Value buffers ---

    # The fields of each record, rich-enum variant, and error code with a
    # payload: class => { field => type }, in wire order.
    RECORDS = {}
    # The variants of each rich enum: module => { tag => variant class }.
    UNIONS = {}

    # Registers the wire layout of a record, a rich-enum variant, or an
    # error code's payload.
    def self.record(cls, **fields)
      if cls.respond_to?(:members) && cls.members != fields.keys
        raise ArgumentError, "#{cls} layout #{fields.keys} doesn't match its members #{cls.members}"
      end

      RECORDS[cls] = fields.freeze
    end

    # Registers a rich enum's variants (each with its `TAG`).
    def self.union(mod, *variants)
      UNIONS[mod] = variants.to_h { |v| [v::TAG, v] }.freeze
    end

    # Appends values in the value-buffer wire format: little-endian, packed,
    # no alignment.
    class Writer
      # `Array#pack` directives of the fixed-width scalars.
      PACK = {
        i8: 'c', u8: 'C', i16: 's<', u16: 'S<', i32: 'l<', u32: 'L<', i64: 'q<', u64: 'Q<', f32: 'e', f64: 'E'
      }.freeze

      def initialize
        @buf = +''.b
        @objects = []
      end

      # Appends `value` as `type`.
      def write(type, value)
        case type
        when :bool then @buf << (value ? "\x01" : "\x00")
        when :string then run(Bridge.string_arg(value))
        when :bytes then run(Bridge.bytes_arg(value))
        when Symbol then @buf << [Bridge.scalar(value, type)].pack(PACK.fetch(type))
        when Array then composite(type, value)
        else user(type, value)
        end
        self
      end

      # Mints the reference each reserved object token carries (just before
      # the encoding is handed over, so an encoding abandoned on the way
      # never strands one), recording it in `minted`, and returns the
      # finished encoding.
      def seal(minted)
        @objects.each do |offset, object|
          ptr = Bridge.clone_ref(object)
          minted << [object.class, ptr]
          @buf[offset, 8] = [ptr.address].pack('Q<')
        end
        @buf
      end

      private

      def run(bytes)
        len(bytes.bytesize)
        @buf << bytes
      end

      def len(count)
        raise RangeError, "#{count} elements don't fit a value buffer" if count > 0xffff_ffff

        @buf << [count].pack('L<')
      end

      def composite(type, value)
        case type[0]
        when :opt
          @buf << (value.nil? ? "\x00" : "\x01")
          write(type[1], value) unless value.nil?
        when :list
          ary = Array.try_convert(value)
          raise TypeError, "expected an Array, got #{value.class}" if ary.nil?

          len(ary.length)
          ary.each { |e| write(type[1], e) }
        when :map
          hash = Hash.try_convert(value)
          raise TypeError, "expected a Hash, got #{value.class}" if hash.nil?

          len(hash.length)
          hash.each do |k, v|
            write(type[1], k)
            write(type[2], v)
          end
        end
      end

      def user(type, value)
        raise TypeError, "expected a #{type.name}, got #{value.class}" unless value.is_a?(type)

        if type.is_a?(Class) && type < Handle
          @objects << [@buf.bytesize, value]
          @buf << [0].pack('Q<')
        elsif UNIONS.key?(type)
          write(:i32, value.class::TAG)
          fields(value.class, value)
        else
          fields(type, value)
        end
      end

      def fields(cls, value)
        RECORDS.fetch(cls).each { |name, type| write(type, value.public_send(name)) }
      end
    end

    # Reads values in the value-buffer wire format, raising NativeBugError
    # on any malformed buffer.
    class Reader
      # The byte width of each fixed-width scalar.
      WIDTHS = { i8: 1, u8: 1, i16: 2, u16: 2, i32: 4, u32: 4, i64: 8, u64: 8, f32: 4, f64: 8 }.freeze

      def initialize(data)
        @data = data
        @pos = 0
      end

      # Reads one `type` value.
      def read(type)
        case type
        when :bool then flag('bool')
        when :string
          text = run.force_encoding(Encoding::UTF_8)
          raise Bridge.malformed('string is not valid UTF-8') unless text.valid_encoding?

          text
        when :bytes then run
        when Symbol then take(WIDTHS.fetch(type), type).unpack1(Writer::PACK.fetch(type))
        when Array then composite(type)
        else user(type)
        end
      end

      # The fields of a record layout as a Hash.
      def fields(cls)
        RECORDS.fetch(cls).transform_values { |type| read(type) }
      end

      def expect_end!
        raise Bridge.malformed('trailing bytes after value') unless @pos == @data.bytesize
      end

      private

      def take(count, what)
        raise Bridge.malformed("truncated #{what}") if @pos + count > @data.bytesize

        bytes = @data.byteslice(@pos, count)
        @pos += count
        bytes
      end

      def flag(what)
        byte = take(1, what).unpack1('C')
        raise Bridge.malformed("#{what} byte out of range") if byte > 1

        byte == 1
      end

      # A u32 element count. Counts aren't checked against the remaining
      # bytes, since an element can encode to zero bytes; a short buffer
      # still fails on its first truncated read.
      def len
        take(4, 'length').unpack1('L<')
      end

      def run
        take(len, 'byte run')
      end

      def composite(type)
        case type[0]
        when :opt then flag('option flag') ? read(type[1]) : nil
        when :list then Array.new(len) { read(type[1]) }
        when :map then Array.new(len) { [read(type[1]), read(type[2])] }.to_h
        end
      end

      def user(type)
        if type.is_a?(Class) && type < Handle
          address = read(:u64)
          raise Bridge.malformed('null object token') if address.zero?

          Bridge.adopt(type, FFI::Pointer.new(address))
        elsif (variants = UNIONS[type])
          tag = read(:i32)
          variant = variants[tag]
          raise Bridge.malformed("unknown #{type.name} tag #{tag}") if variant.nil?

          variant.new(**fields(variant))
        else
          type.new(**fields(type))
        end
      end
    end

    # The error for a malformed value the library handed over.
    def self.malformed(what)
      NativeBugError.new("malformed value buffer: #{what}", code: Error::MARSHAL)
    end

    # A writer holding `value` encoded as `type`, sealed just before the
    # call (see #seal).
    def self.encode(type, value)
      Writer.new.write(type, value)
    end

    # Seals the writers of one call's value-buffer arguments and returns
    # their encodings. If any reference can't be minted (a closed wrapper),
    # the ones already minted are released before the error propagates. The
    # library adopts every minted reference once the call is made.
    def self.seal(*writers)
      minted = []
      writers.map { |writer| writer.seal(minted) }
    rescue Exception # rubocop:disable Lint/RescueException
      minted.each { |cls, ptr| INTERFACES.fetch(cls)[1].call(ptr) }
      raise
    end

    # Decodes a returned value buffer: copies and releases it, then reads
    # one `type` value, which must consume every byte.
    def self.decode(ptr, len, type)
      decode_bytes(take_bytes(ptr, len), type)
    end

    # Decodes a borrowed value buffer (a callback argument) without
    # releasing it.
    def self.decode_borrowed(ptr, len, type)
      decode_bytes(borrow_bytes(ptr, len), type)
    end

    # Reads one `type` value (or, with `fields`, a layout's fields) from a
    # complete encoding.
    def self.decode_bytes(bytes, type, fields: false)
      reader = Reader.new(bytes)
      value = fields ? reader.fields(type) : reader.read(type)
      reader.expect_end!
      value
    end

    # --- Callback interfaces ---

    # Ruby objects the library refers to by an integer key: callback
    # implementations (the vtable `ctx`) and pending async calls (the
    # completion `context`). The library never sees a Ruby object address.
    # A Monitor rather than a Mutex, so a finalizer that runs while this
    # thread holds the lock can't deadlock it.
    class Handles
      def initialize
        @entries = {}
        @next = 0
        @lock = Monitor.new
      end

      def put(value)
        @lock.synchronize do
          @next += 1
          @entries[@next] = value
          FFI::Pointer.new(@next)
        end
      end

      def get(key)
        @lock.synchronize { @entries[key.address] }
      end

      def take(key)
        @lock.synchronize { @entries.delete(key.address) }
      end
    end
    HANDLES = Handles.new

    # The vtable `free` every interface shares: drops the implementation
    # when the library releases its last reference (on any library thread).
    FREE = FFI::Function.new(:void, [:pointer]) { |ctx| HANDLES.take(ctx) }

    # The vtable of each callback interface module.
    VTABLES = {}

    # Builds `iface`'s one process-wide vtable: the `{size, flags, free}`
    # header (flags 0: methods may run on any thread), then one trampoline
    # per method in declaration order. The trampolines live as long as the
    # vtable, which is the process.
    def self.vtable(iface, *methods)
      word = FFI::Pointer.size
      size = 8 + (word * (methods.length + 1))
      table = FFI::MemoryPointer.new(:uint8, size)
      table.put_uint32(0, size)
      table.put_uint32(4, 0)
      table.put_pointer(8, FREE)
      methods.each_with_index { |fn, i| table.put_pointer(8 + (word * (i + 1)), fn) }
      VTABLES[iface] = [table, methods.freeze].freeze
    end

    # Registers a callback implementation and returns its `ctx` and
    # `vtable` arguments: two NULLs for a nil (optional) implementation.
    # Never raises, so nothing can strand the registration between this
    # and the call that hands it to the library (an unusable implementation
    # fails when the library calls it).
    def self.register(impl, iface)
      return [nil, nil] if impl.nil?

      [HANDLES.put([:callback, impl]), VTABLES.fetch(iface)[0]]
    end

    # Raises TypeError for a nil callback implementation where one is
    # required.
    def self.present!(value, name)
      raise TypeError, "#{name} must not be nil" if value.nil?
    end

    # Resolves a trampoline's `ctx` back to the registered implementation.
    def self.impl(ctx)
      kind, impl = HANDLES.get(ctx)
      raise Error.new('callback context is not registered', code: Error::CALLBACK) unless kind == :callback

      impl
    end

    # Runs a trampoline's body. Nothing unwinds through the C frame: an
    # exception is reported through `out_err` (see report) and the method
    # returns `default`.
    def self.callback(out_err, error, default = nil)
      yield
    rescue Exception => e # rubocop:disable Lint/RescueException
      report(out_err, e, error)
      default
    end

    # Reports a callback implementation's exception through `out_err`. A
    # method that throws a domain (`error`) reports one of its errors with
    # the error's code and fields as the payload; any other exception is -1
    # (or -4 for a method that declares no errors) with its message. The
    # library copies both.
    def self.report(out_err, exception, error)
      code, payload = report_code(exception, error)
      set_error(out_err, code, exception)
      Native.{{PREFIX}}_error_set_payload(out_err, payload, payload.bytesize) unless payload.nil? || payload.empty?
    rescue Exception => e # rubocop:disable Lint/RescueException
      set_error(out_err, error.nil? ? Error::CALLBACK : Error::GENERIC, e)
    end

    def self.report_code(exception, error)
      return [Error::CALLBACK, nil] if error.nil?
      return [Error::GENERIC, nil] unless error != Error && exception.is_a?(error) && exception.code.positive?

      code_class = code_class(exception.class)
      return [exception.code, nil] unless RECORDS.key?(code_class)

      writer = Writer.new
      RECORDS[code_class].each { |name, type| writer.write(type, exception.public_send(name)) }
      [exception.code, seal(writer).first]
    end

    def self.set_error(out_err, code, exception)
      message = exception.message.to_s
      message = exception.class.name.to_s if message.empty?
      message = message.encode(Encoding::UTF_8, invalid: :replace, undef: :replace).b
      Native.{{PREFIX}}_error_set(out_err, code, message, message.bytesize)
    end

    # Hands a callback's string, bytes, or buffer return to the library: a
    # run allocated with {{PREFIX}}_alloc, written to the out slots. The
    # library adopts and frees it.
    def self.run_return(bytes, out_ptr, out_len)
      out_ptr.write_pointer(alloc(bytes.bytesize) { |run| run.put_bytes(0, bytes) })
      out_len.write(:size_t, bytes.bytesize)
      nil
    end

    # A callback's string return.
    def self.string_return(value, out_ptr, out_len)
      run_return(string_arg(value), out_ptr, out_len)
    end

    # A callback's bytes return.
    def self.bytes_return(value, out_ptr, out_len)
      run_return(bytes_arg(value), out_ptr, out_len)
    end

    # A callback's value-buffer return.
    def self.buffer_return(type, value, out_ptr, out_len)
      run_return(seal(encode(type, value)).first, out_ptr, out_len)
    end

    # A callback's typed-array return, in a run the library adopts.
    def self.slice_return(values, kind, out_ptr, out_len)
      ary = Array.try_convert(values)
      raise TypeError, "expected an Array, got #{values.class}" if ary.nil?

      checked = ary.map { |v| scalar(v, kind) }
      run = alloc(checked.length * SIZES.fetch(kind)) do |ptr|
        ptr.public_send(:"put_array_of_#{FFI_TYPES.fetch(kind)}", 0, checked)
      end
      out_ptr.write_pointer(run)
      out_len.write(:size_t, checked.length)
      nil
    end

    # A callback's optional-scalar return: writes a present value to the
    # out slot and returns whether there is one.
    def self.opt_return(value, kind, out_value)
      return false if value.nil?

      out_value.put(FFI_TYPES.fetch(kind), 0, scalar(value, kind))
      true
    end

    # A callback's object return: a fresh strong reference the library
    # adopts, or NULL for nil (which the library rejects where the return
    # isn't optional).
    def self.object_return(value, cls)
      return nil if value.nil?
      raise TypeError, "expected a #{cls.name}, got #{value.class}" unless value.is_a?(cls)

      clone_ref(value)
    end

    # A {{PREFIX}}_alloc run of `len` bytes, filled by the block (NULL when
    # empty).
    def self.alloc(len)
      return nil if len.zero?

      run = Native.{{PREFIX}}_alloc(len)
      raise NoMemoryError, '{{PREFIX}}_alloc failed' if run.null?

      yield run
      run
    end

    # --- Load-time checks and diagnostics ---

    # Checks, for every top-level module, that the library's contract table
    # (`{{PREFIX}}_{module}_contract`) holds each declaration these bindings
    # were generated with, as `[id, hash, path]`, with an equal signature
    # hash. Declarations the library adds are fine.
    def self.check_contract!(tables)
      tables.each do |symbol, expected|
        begin
          Native.attach_function(symbol, [:pointer], :pointer) unless Native.respond_to?(symbol)
        rescue FFI::NotFoundError
          raise {{MODULE}}::LoadError, "the loaded {{LIBRARY}} library does not export #{symbol}"
        end
        out_len = FFI::MemoryPointer.new(:size_t)
        table = Native.public_send(symbol, out_len)
        count = table.null? ? 0 : out_len.read(:size_t)
        actual = Array.new(count) do |i|
          entry = Native::ContractEntry.new(table + (i * Native::ContractEntry.size))
          [entry[:id], entry[:hash]]
        end.to_h
        expected.each do |id, hash, path|
          found = actual[id]
          raise {{MODULE}}::LoadError, "{{LIBRARY}}: #{path} is missing from the library" if found.nil?
          next if found == hash

          raise {{MODULE}}::LoadError, "{{LIBRARY}}: #{path} changed since these bindings were generated"
        end
      end
    end

    # Live native resources of one kind (0 objects, 1 callbacks, 2
    # iterators, 3 cancel tokens, 4 byte runs; -1 reports whether the
    # library counts at all). Only libraries built with the `leak-check`
    # feature count.
    def self.debug_live(kind)
      unless Native.respond_to?(:{{PREFIX}}_debug_live)
        begin
          Native.attach_function(:{{PREFIX}}_debug_live, [:int32], :uint64)
        rescue FFI::NotFoundError
          raise Error, '{{LIBRARY}} does not export {{PREFIX}}_debug_live'
        end
      end
      Native.{{PREFIX}}_debug_live(kind)
    end
  end
  private_constant :Bridge
end
