# Vtable entries of callback methods that return a structure (a
# {{PREFIX}}_run or an optional scalar) by value. A ctypes callback can't
# return a structure, so each such entry is a libffi closure, built through
# the libffi that ctypes itself runs on: libffi calls one ctypes callback with
# the return slot and the argument addresses, and it converts the arguments,
# calls the trampoline, and stores the structure the trampoline returns.


class _FfiType(ctypes.Structure):
    """libffi's `ffi_type`."""


_FfiType._fields_ = [
    ("size", ctypes.c_size_t),
    ("alignment", ctypes.c_ushort),
    ("type", ctypes.c_ushort),
    ("elements", ctypes.POINTER(ctypes.POINTER(_FfiType))),
]

# libffi's `FFI_TYPE_STRUCT`.
_FFI_TYPE_STRUCT = 13

# The libffi type of each ctypes scalar a vtable entry's slot can have (a
# C `bool` is one byte).
_FFI_SCALARS: dict[Any, str] = {
    ctypes.c_bool: "uint8",
    ctypes.c_int8: "sint8",
    ctypes.c_uint8: "uint8",
    ctypes.c_int16: "sint16",
    ctypes.c_uint16: "uint16",
    ctypes.c_int32: "sint32",
    ctypes.c_uint32: "uint32",
    ctypes.c_int64: "sint64",
    ctypes.c_uint64: "uint64",
    ctypes.c_float: "float",
    ctypes.c_double: "double",
    ctypes.c_void_p: "pointer",
}

# `void handler(ffi_cif* cif, void* ret, void** args, void* user_data)`.
_FfiHandler = ctypes.CFUNCTYPE(
    None, ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p
)

_ffi_lock = threading.Lock()
_ffi_found: list[ctypes.CDLL] = []
_ffi_types: dict[Any, _FfiType] = {}
# Every closure's signature, handler, and type tables, kept for the process
# lifetime.
_ffi_keep: list[Any] = []


def _ffi_library() -> ctypes.CDLL:
    """The libffi ctypes runs on: found through the `_ctypes` extension, which
    links it, or by name."""
    if _ffi_found:
        return _ffi_found[0]
    import _ctypes
    import ctypes.util

    names = [
        getattr(_ctypes, "__file__", None),
        ctypes.util.find_library("ffi"),
        "libffi.so.8",
        "libffi.so.7",
        "libffi-8.dll",
        "libffi-7.dll",
    ]
    for name in names:
        if not name:
            continue
        try:
            lib = ctypes.CDLL(name)
        except OSError:
            continue
        if not hasattr(lib, "ffi_prep_closure_loc"):
            continue
        lib.ffi_prep_cif.restype = ctypes.c_int
        lib.ffi_prep_cif.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_uint,
            ctypes.POINTER(_FfiType),
            ctypes.POINTER(ctypes.POINTER(_FfiType)),
        ]
        lib.ffi_closure_alloc.restype = ctypes.c_void_p
        lib.ffi_closure_alloc.argtypes = [ctypes.c_size_t, ctypes.POINTER(ctypes.c_void_p)]
        lib.ffi_prep_closure_loc.restype = ctypes.c_int
        lib.ffi_prep_closure_loc.argtypes = [
            ctypes.c_void_p,
            ctypes.c_void_p,
            _FfiHandler,
            ctypes.c_void_p,
            ctypes.c_void_p,
        ]
        _ffi_found.append(lib)
        return lib
    raise OSError(
        "cannot find libffi, which a callback method returning a string, bytes, "
        "a typed array, a value buffer, or an optional scalar needs"
    )


def _ffi_abi() -> int:
    """libffi's `FFI_DEFAULT_ABI` on the 64-bit platforms wheels target."""
    x86 = platform.machine().lower() in ("x86_64", "amd64")
    if os.name == "nt":
        return 1 if x86 else 2
    return 2 if x86 else 1


def _ffi_type(t: Any) -> _FfiType:
    """The libffi type of a ctypes scalar or structure."""
    found = _ffi_types.get(t)
    if found is None:
        if isinstance(t, type) and issubclass(t, ctypes.Structure):
            fields = [_ffi_type(field[1]) for field in t._fields_]
            elements = (ctypes.POINTER(_FfiType) * (len(fields) + 1))(
                *[ctypes.pointer(field) for field in fields]
            )
            # libffi computes the size and alignment.
            found = _FfiType(0, 0, _FFI_TYPE_STRUCT, elements)
            _ffi_keep.append(elements)
        else:
            found = _FfiType.in_dll(_ffi_library(), "ffi_type_" + _FFI_SCALARS[t])
        _ffi_types[t] = found
    return found


def _ffi_arg(t: Any, address: int) -> Any:
    """One argument as a ctypes callback would receive it: a scalar's value,
    or a copy of a structure."""
    if isinstance(t, type) and issubclass(t, ctypes.Structure):
        return t.from_buffer_copy(ctypes.string_at(address, ctypes.sizeof(t)))
    return t.from_address(address).value


def _vtable_struct_entry(fn: Callable[..., Any], restype: Any, *argtypes: Any) -> int:
    """The address of a function the producer calls as one vtable entry
    returning the structure `restype`."""
    with _ffi_lock:
        lib = _ffi_library()
        atypes = (ctypes.POINTER(_FfiType) * len(argtypes))(
            *[ctypes.pointer(_ffi_type(t)) for t in argtypes]
        )
        rtype = ctypes.pointer(_ffi_type(restype))
        # An `ffi_cif` and an `ffi_closure`, each with room to spare.
        cif = (ctypes.c_uint64 * 16)()
        if lib.ffi_prep_cif(cif, _ffi_abi(), len(argtypes), rtype, atypes) != 0:
            raise OSError("libffi rejected a callback method's signature")
        code = ctypes.c_void_p()
        closure = lib.ffi_closure_alloc(256, ctypes.byref(code))
        if not closure:
            raise MemoryError("ffi_closure_alloc failed")
        size = ctypes.sizeof(restype)

        def handler(cif: int | None, ret: Any, args: Any, data: int | None) -> None:
            values = [_ffi_arg(t, args[i]) for i, t in enumerate(argtypes)]
            result = fn(*values)
            ctypes.memmove(ret, ctypes.addressof(result), size)

        entry = _FfiHandler(handler)
        if lib.ffi_prep_closure_loc(closure, cif, entry, None, code) != 0:
            raise OSError("libffi couldn't prepare a callback method")
        _ffi_keep.extend([atypes, rtype, cif, entry])
        return code.value or 0
