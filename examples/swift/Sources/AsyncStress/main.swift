// Async stress test for the Swift async lifecycle.
//
// Loads the async-demo cdylib (path in ASYNC_DEMO_LIB) via dlopen and
// spawns 1000 concurrent calls to weaveffi_tasks_run_n_tasks_async,
// mirroring the Unmanaged.passRetained / .takeRetainedValue continuation
// pattern that the Swift generator emits.
//
// Verifies:
//   * every spawned worker fires its callback exactly once
//   * each callback returns the n value passed in
//   * after awaiting all calls, weaveffi_tasks_active_callbacks returns 0
//
// Prints "OK" and exits 0 on success.

import Foundation
#if canImport(Darwin)
import Darwin
#elseif canImport(Glibc)
import Glibc
#endif

let nTasks = 1000

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

guard let libPath = ProcessInfo.processInfo.environment["ASYNC_DEMO_LIB"] else {
    FileHandle.standardError.write(Data("ASYNC_DEMO_LIB not set\n".utf8))
    exit(1)
}
let lib = mustOpen(libPath)

typealias RunNTasksCb = @convention(c) (UnsafeMutableRawPointer?, UnsafeMutableRawPointer?, Int32) -> Void
typealias RunNTasksAsync = @convention(c) (Int32, RunNTasksCb, UnsafeMutableRawPointer?) -> Void
typealias ActiveCallbacks = @convention(c) (UnsafeMutableRawPointer?) -> Int64

let runNTasksAsync = mustSym(lib, "weaveffi_tasks_run_n_tasks_async", as: RunNTasksAsync.self)
let activeCallbacks = mustSym(lib, "weaveffi_tasks_active_callbacks", as: ActiveCallbacks.self)

final class Slot {
    let cont: CheckedContinuation<Int32, Never>
    init(_ c: CheckedContinuation<Int32, Never>) { self.cont = c }
}

let cb: RunNTasksCb = { context, _, result in
    let slot = Unmanaged<Slot>.fromOpaque(context!).takeRetainedValue()
    slot.cont.resume(returning: result)
}

func runOne(_ n: Int32) async -> Int32 {
    await withCheckedContinuation { (continuation: CheckedContinuation<Int32, Never>) in
        let ctx = Unmanaged.passRetained(Slot(continuation)).toOpaque()
        runNTasksAsync(n, cb, ctx)
    }
}

let start = Date()
let results: [Int32] = await withTaskGroup(of: (Int, Int32).self, returning: [Int32].self) { group in
    for i in 0..<nTasks {
        group.addTask { (i, await runOne(Int32(i))) }
    }
    var out = [Int32](repeating: -1, count: nTasks)
    for await (i, v) in group {
        out[i] = v
    }
    return out
}

for i in 0..<nTasks {
    if results[i] != Int32(i) {
        FileHandle.standardError.write(Data("results[\(i)] = \(results[i]), expected \(i)\n".utf8))
        exit(1)
    }
}

let active = activeCallbacks(nil)
if active != 0 {
    FileHandle.standardError.write(Data("active_callbacks = \(active) (expected 0)\n".utf8))
    exit(1)
}

let elapsed = Date().timeIntervalSince(start)
print(String(format: "OK (%d tasks in %.2fs)", nTasks, elapsed))
