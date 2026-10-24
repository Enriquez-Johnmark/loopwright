//! The Excel parser child on its own, for the engine's tests. Products run
//! the child from their own binary instead (`nudgewright::sheets::run_child`).

use std::process::ExitCode;

fn main() -> ExitCode {
    nudgewright::sheets::run_child()
}
