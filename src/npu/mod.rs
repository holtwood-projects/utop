//! Ascend NPU monitoring.
//!
//! Layering mirrors the process side of the crate:
//!
//! | module   | responsibility                                  |
//! |----------|-------------------------------------------------|
//! | `model`  | pure data types, aggregation, sorting/filtering |
//! | `smi`    | `npu-smi` invocation and text parsing           |
//! | `procfs` | `/proc` enrichment and container attribution    |
//! | `report` | one-shot `--once` output                        |
//!
//! `smi` spawns `npu-smi`; `procfs` reads `/proc` and spawns `docker ps`;
//! everything else is pure.

pub mod collect;
pub mod model;
pub mod procfs;
pub mod report;
pub mod smi;

pub use collect::{NO_CARD, NpuCollector, NpuFeed};
pub use model::{
    CardFlag, Health, NpuCard, NpuProcRow, NpuProcess, NpuSnapshot, ProcExtra, aggregate_processes,
    card_flag, filter_proc_rows, sort_proc_rows,
};
pub use smi::{SmiError, collect};
