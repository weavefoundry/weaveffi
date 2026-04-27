const isWorker = typeof document === "undefined";

if (isWorker) {
  const apiPromise = (async () => {
    const { loadWeaveffiWasm } = await import("../../generated/wasm/weaveffi_wasm.js");
    const wasmUrl = new URL(
      "../../../target/wasm32-unknown-unknown/release/calculator.wasm",
      import.meta.url,
    );

    return loadWeaveffiWasm(wasmUrl);
  })();

  self.onmessage = async ({ data }) => {
    const { id, op, args = {} } = data;

    try {
      const api = await apiPromise;
      let value;

      switch (op) {
        case "add":
          value = api.calculator.add(args.a, args.b);
          break;
        case "echo":
          value = api.calculator.echo(args.s);
          break;
        case "divideByZero":
          value = api.calculator.div(1, 0);
          break;
        default:
          throw new Error(`Unknown operation: ${op}`);
      }

      self.postMessage({ id, ok: true, value });
    } catch (error) {
      self.postMessage({
        id,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      });
    }
  };
} else {
  const worker = new Worker(new URL("./app.js?worker", import.meta.url), { type: "module" });
  const pending = new Map();
  let nextId = 1;

  const $ = (id) => document.getElementById(id);

  function setStatus(message, isError = false) {
    $("status").textContent = message;
    $("status").classList.toggle("error", isError);
  }

  function callWorker(op, args) {
    return new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      worker.postMessage({ id, op, args });
    });
  }

  function checkedNumber(inputId) {
    const value = $(inputId).valueAsNumber;
    if (!Number.isFinite(value)) {
      throw new Error(`${inputId} must be a number`);
    }
    return value;
  }

  async function run(action) {
    try {
      setStatus("Running...");
      await action();
      setStatus("Ready");
    } catch (error) {
      setStatus(error instanceof Error ? error.message : String(error), true);
    }
  }

  worker.onmessage = ({ data }) => {
    const callbacks = pending.get(data.id);
    if (!callbacks) {
      return;
    }

    pending.delete(data.id);
    if (data.ok) {
      callbacks.resolve(data.value);
    } else {
      callbacks.reject(new Error(data.error));
    }
  };

  worker.onerror = (event) => {
    setStatus(`Worker error: ${event.message}`, true);
  };

  $("add-button").addEventListener("click", () =>
    run(async () => {
      const a = checkedNumber("add-a");
      const b = checkedNumber("add-b");
      $("add-output").textContent = `${a} + ${b} = ${await callWorker("add", { a, b })}`;
    }),
  );

  $("echo-button").addEventListener("click", () =>
    run(async () => {
      const s = $("echo-input").value;
      $("echo-output").textContent = await callWorker("echo", { s });
    }),
  );

  $("error-button").addEventListener("click", () =>
    run(async () => {
      try {
        const result = await callWorker("divideByZero");
        $("error-output").textContent = `Unexpected success: ${result}`;
      } catch (error) {
        $("error-output").textContent = error instanceof Error ? error.message : String(error);
      }
    }),
  );

  callWorker("add", { a: 0, b: 0 })
    .then(() => setStatus("Ready"))
    .catch((error) => setStatus(error instanceof Error ? error.message : String(error), true));
}
