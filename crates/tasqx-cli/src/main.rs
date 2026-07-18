//! The `tasqx` binary — a shim.
//!
//! All of the CLI (the clap surface, the `run_*` dispatchers, the backend
//! selection) lives in the library half of this crate, `tasqx_cli`. It sits
//! there and not here because a binary-only crate exports nothing: the
//! integration tests under `tests/` could not import `cmddoc::COMMAND_REF`,
//! so the executable-examples guard had to hand-copy the list of examples it
//! ran — a copy that silently drifted until fourteen of the twenty-seven
//! `RunKind::Safe` examples were being executed by nothing at all. With a lib
//! target the guard iterates the source of truth instead.

fn main() {
    tasqx_cli::run();
}
