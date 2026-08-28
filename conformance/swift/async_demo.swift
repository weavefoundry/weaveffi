// Conformance consumer: async-demo sample, Swift target.
//
// Drives the CheckedContinuation-backed async surface end to end:
// `Tasks.runTask` as a throwing async static resumed from the producer's
// worker thread and decoded from a value buffer into the plain `TaskResult`
// struct, the typed `TaskError.invalidName` case (with its stable
// `errorCode`) thrown for an empty name, the buffered list-of-records round
// trip through the non-throwing async `runBatch`, the direct-scalar
// `runNTasks`, the sync `cancelTask`, and `activeCallbacks` settling to zero
// once every task body has completed.

import Foundation
import AsyncDemo

func fail(_ msg: String) -> Never {
    FileHandle.standardError.write(Data("assertion failed: \(msg)\n".utf8))
    exit(1)
}

func expect(_ cond: Bool, _ msg: String) {
    if !cond { fail(msg) }
}

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
    guard case .invalidName = e else {
        fail("expected .invalidName (got \(e))")
    }
    expect(e.errorCode == 1, "invalidName carries code 1 (got \(e.errorCode))")
} catch {
    fail("expected TaskError (got \(error))")
}

// Buffered list-of-records both ways through the non-throwing async.
let batch = await Tasks.runBatch(names: ["a", "b", "c"])
expect(
    batch.map { $0.value } == ["completed: a", "completed: b", "completed: c"],
    "runBatch values"
)
expect(batch.allSatisfy { $0.success }, "runBatch success flags")

// Direct scalar through the async callback.
let n = await Tasks.runNTasks(n: 7)
expect(n == 7, "runNTasks echoes n")

// Sync functions beside the async ones.
expect(!Tasks.cancelTask(id: 1), "cancelTask reports not cancelled")

// Every spawned task body has completed by the time its callback fires.
expect(Tasks.activeCallbacks() == 0, "activeCallbacks settles to zero")

print("swift async-demo conformance: OK")
