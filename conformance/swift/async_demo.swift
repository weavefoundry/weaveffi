// Conformance consumer: async-demo sample, Swift target.
//
// Drives the checked-continuation async surface end to end: `Tasks.runTask`
// as a throwing async static resumed from the producer's worker thread and
// decoded from a value buffer into the `TaskResult` struct, the typed
// `TaskError.invalidName` case thrown for an empty name, the buffered
// list-of-records round trip through the non-throwing async `runBatch`, and
// the direct-scalar `runNTasks`. The cancellable `wait` completes after its
// timeout when left alone; cancelling the awaiting task cancels the native
// token, so the call ends at once with `CancellationError`, including when
// the task was cancelled before the call started. `activeCallbacks` settles
// to zero, and at exit every cancel token and allocation has been released.

import CAsyncDemo
import AsyncDemo
import Foundation

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

/// Every live-object counter the producer keeps must settle at zero.
func assertNoLeaks() {
    let kinds = ["objects", "callbacks", "iterators", "cancel tokens", "allocations"]
    var live: [UInt64] = []
    for _ in 0..<200 {
        live = (0..<5).map { async_demo_debug_live(Int32($0)) }
        if live.allSatisfy({ $0 == 0 }) { return }
        usleep(10_000)
    }
    for (kind, n) in live.enumerated() where n != 0 {
        fail("\(n) live \(kinds[kind]) at exit")
    }
}

/// Await `task`, which must end with `CancellationError` within five seconds.
func expectCancelled(_ task: Task<Int64, Error>, _ what: String) async {
    let start = Date()
    do {
        let value = try await task.value
        fail("\(what): expected CancellationError, got \(value)")
    } catch is CancellationError {
    } catch {
        fail("\(what): expected CancellationError, got \(error)")
    }
    expect(Date().timeIntervalSince(start) < 5, "\(what): didn't wait for the timeout")
}

func run() async {
    // Async record return: resumed with a TaskResult decoded from the
    // completion callback's value buffer.
    do {
        let result = try await Tasks.runTask(name: "alpha")
        expect(result.id > 0, "runTask assigns an id")
        expect(result.value == "completed: alpha", "runTask value (got \(result.value))")
        expect(result.success, "runTask success flag")
    } catch {
        fail("runTask threw unexpectedly: \(error)")
    }

    // Typed async error: the empty name resumes by throwing the typed case.
    do {
        _ = try await Tasks.runTask(name: "")
        fail("expected TaskError.invalidName for empty name")
    } catch let e as TaskError {
        guard case let .invalidName(message) = e else { fail("expected .invalidName (got \(e))") }
        expect(e.errorCode == 1, "invalidName carries code 1 (got \(e.errorCode))")
        expect(message == "task name must not be empty", "invalidName message (got \(message))")
    } catch {
        fail("expected TaskError (got \(error))")
    }

    // Buffered list-of-records both ways through the non-throwing async.
    let batch = await Tasks.runBatch(names: ["a", "b", "c"])
    expect(batch.map { $0.value } == ["completed: a", "completed: b", "completed: c"], "runBatch values")
    expect(batch.allSatisfy { $0.success }, "runBatch success flags")
    expect(await Tasks.runBatch(names: []).isEmpty, "runBatch of nothing")

    // Direct scalar through the async callback, many in flight at once.
    let sums = await withTaskGroup(of: Int32.self) { group in
        for i in 0..<32 {
            group.addTask { await Tasks.runNTasks(n: Int32(i)) }
        }
        var total: Int32 = 0
        for await n in group { total += n }
        return total
    }
    expect(sums == (0..<32).reduce(0, +), "runNTasks echoes n (sum \(sums))")

    // Cancellable: left alone, `wait` completes after its timeout.
    do {
        let waited = try await Tasks.wait(timeoutMs: 20)
        expect(waited == 20, "wait completes with its timeout (got \(waited))")
    } catch {
        fail("uncancelled wait threw: \(error)")
    }

    // Cancelling the awaiting task cancels the native token mid-call.
    let running = Task { try await Tasks.wait(timeoutMs: 60_000) }
    try? await Task.sleep(nanoseconds: 50_000_000)
    running.cancel()
    await expectCancelled(running, "cancel mid-call")

    // A task cancelled before the call starts never runs the body.
    let early = Task { () async throws -> Int64 in
        while !Task.isCancelled { await Task.yield() }
        return try await Tasks.wait(timeoutMs: 60_000)
    }
    early.cancel()
    await expectCancelled(early, "cancel before the call")

    // Every spawned task body finishes, cancelled ones included.
    for _ in 0..<100 where Tasks.activeCallbacks() != 0 {
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
    expect(Tasks.activeCallbacks() == 0, "activeCallbacks settles to zero")
}

await run()
assertNoLeaks()
print("swift/async-demo: OK")
