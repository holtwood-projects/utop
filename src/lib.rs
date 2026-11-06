//! utop — a lightweight htop clone built with ratatui and sysinfo.
//!
//! Module map:
//!
//! | module    | responsibility                                          |
//! |-----------|---------------------------------------------------------|
//! | `cli`     | command-line argument parsing (pure parser)              |
//! | `model`   | process row model, sorting, filtering, tree (pure)       |
//! | `app`     | application state and input modes (pure)                 |
//! | `collect` | sysinfo snapshots — the only module that talks to sysinfo |
//! | `ui`      | ratatui rendering (pure: reads only `App` snapshots)     |
//! | `run`     | event loop — the impure shell owning the terminal        |
//! | `npu`     | Ascend NPU model, `npu-smi` parsing and collector thread |
//!
//! The layering is strict and acyclic: `model` ← `cli` ← `app` ←
//! {`collect`, `ui`} ← `run`. Everything except `run` and `collect` is pure
//! and unit-tested without a live system.

pub mod cli;
pub mod model;
pub mod npu;

mod app;
mod collect;
mod run;
mod ui;

use std::error::Error;

/// Runs the TUI, or one-shot NPU output when `--once` is set.
pub fn run(config: &cli::Config) -> Result<(), Box<dyn Error>> {
    if config.once {
        return npu::report::once(config);
    }
    run::run(config)
}
