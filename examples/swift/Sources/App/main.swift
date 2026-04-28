// End-to-end consumer test for the Swift binding consumers.
//
// Loads the calculator and contacts cdylibs at runtime via dlopen and
// exercises a representative slice of the C ABI: add, create_contact,
// list_contacts, delete_contact. Prints "OK" and exits 0 on success;
// any assertion failure exits 1.

import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif

func mustOpen(_ path: String) -> UnsafeMutableRawPointer {
    guard let h = dlopen(path, RTLD_NOW | RTLD_GLOBAL) else {
        let msg = dlerror().map { String(cString: $0) } ?? "unknown"
        FileHandle.standardError.write(Data("dlopen(\(path)): \(msg)\n".utf8))
        exit(1)
    }
    return h
}

func mustSym<T>(_ lib: UnsafeMutableRawPointer, _ name: String, as: T.Type) -> T {
    guard let p = dlsym(lib, name) else {
        let msg = dlerror().map { String(cString: $0) } ?? "unknown"
        FileHandle.standardError.write(Data("dlsym(\(name)): \(msg)\n".utf8))
        exit(1)
    }
    return unsafeBitCast(p, to: T.self)
}

func check(_ cond: Bool, _ msg: String) {
    if !cond {
        FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
        exit(1)
    }
}

// 16 bytes: int32 code + 4 bytes padding + 8 byte pointer
let errorSize = 16
func newError() -> UnsafeMutableRawPointer {
    let p = UnsafeMutableRawPointer.allocate(byteCount: errorSize, alignment: 8)
    p.initializeMemory(as: UInt8.self, repeating: 0, count: errorSize)
    return p
}

func errorCode(_ p: UnsafeMutableRawPointer) -> Int32 { p.load(as: Int32.self) }

guard let calcPath = ProcessInfo.processInfo.environment["WEAVEFFI_LIB"],
      let contactsPath = ProcessInfo.processInfo.environment["CONTACTS_LIB"]
else {
    FileHandle.standardError.write(Data("WEAVEFFI_LIB and CONTACTS_LIB must be set\n".utf8))
    exit(1)
}

let calc = mustOpen(calcPath)
let contacts = mustOpen(contactsPath)

typealias AddFn = @convention(c) (Int32, Int32, UnsafeMutableRawPointer) -> Int32
typealias CreateFn = @convention(c) (
    UnsafePointer<CChar>?, UnsafePointer<CChar>?, UnsafePointer<CChar>?,
    Int32, UnsafeMutableRawPointer
) -> UInt64
typealias ListFn = @convention(c) (
    UnsafeMutablePointer<Int>, UnsafeMutableRawPointer
) -> UnsafeMutablePointer<UnsafeMutableRawPointer?>?
typealias GetIdFn = @convention(c) (UnsafeRawPointer) -> Int64
typealias ListFreeFn = @convention(c) (UnsafeMutablePointer<UnsafeMutableRawPointer?>?, Int) -> Void
typealias DeleteFn = @convention(c) (UInt64, UnsafeMutableRawPointer) -> Int32
typealias CountFn = @convention(c) (UnsafeMutableRawPointer) -> Int32

let add = mustSym(calc, "weaveffi_calculator_add", as: AddFn.self)
let create = mustSym(contacts, "weaveffi_contacts_create_contact", as: CreateFn.self)
let list = mustSym(contacts, "weaveffi_contacts_list_contacts", as: ListFn.self)
let getID = mustSym(contacts, "weaveffi_contacts_Contact_get_id", as: GetIdFn.self)
let listFree = mustSym(contacts, "weaveffi_contacts_Contact_list_free", as: ListFreeFn.self)
let del = mustSym(contacts, "weaveffi_contacts_delete_contact", as: DeleteFn.self)
let count = mustSym(contacts, "weaveffi_contacts_count_contacts", as: CountFn.self)

let err = newError()
defer { err.deallocate() }

let sum = add(2, 3, err)
check(errorCode(err) == 0, "calculator_add error")
check(sum == 5, "calculator_add(2,3) != 5")

err.initializeMemory(as: UInt8.self, repeating: 0, count: errorSize)
let h = "Alice".withCString { f in
    "Smith".withCString { l in
        "alice@example.com".withCString { e in
            create(f, l, e, 0, err)
        }
    }
}
check(errorCode(err) == 0, "create_contact error")
check(h != 0, "create_contact returned 0")

err.initializeMemory(as: UInt8.self, repeating: 0, count: errorSize)
var len: Int = 0
let items = list(&len, err)
check(errorCode(err) == 0, "list_contacts error")
check(len == 1, "list_contacts length != 1")
check(items != nil, "list_contacts null")
let firstPtr = items![0]!
check(getID(firstPtr) == Int64(h), "id mismatch")
listFree(items, len)

err.initializeMemory(as: UInt8.self, repeating: 0, count: errorSize)
let deleted = del(h, err)
check(errorCode(err) == 0, "delete_contact error")
check(deleted == 1, "delete_contact did not return 1")

err.initializeMemory(as: UInt8.self, repeating: 0, count: errorSize)
check(count(err) == 0, "store not empty after cleanup")

print("OK")
