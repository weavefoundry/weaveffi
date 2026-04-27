//! Cross-platform helper binary used by codegen hook integration tests.
//!
//! Exits 0 when `argv[1] == "ok"` and exits 1 otherwise so the hook tests
//! can exercise both success and failure paths without depending on
//! `sh` / `cmd.exe` shell builtins.

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_default();
    if arg == "ok" {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}
