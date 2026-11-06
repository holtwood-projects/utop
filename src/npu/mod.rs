//! Ascend NPU monitoring.
//!
//! Layering mirrors the process side of the crate:
//!
//! | module   | responsibility                                  |
//! |----------|-------------------------------------------------|
//! | `model`  | pure data types, aggregation, sorting/filtering |
//! | `smi`    | `npu-smi` invocation and text parsing           |
//! | `report` | one-shot `--once` output                        |
//!
//! Only `smi` spawns a subprocess; everything else is pure.

pub mod collect;
pub mod model;
pub mod report;
pub mod smi;

pub use collect::NpuFeed;
pub use model::{
    Health, NpuCard, NpuProcRow, NpuProcess, NpuSnapshot, aggregate_processes, filter_proc_rows,
    sort_proc_rows,
};
pub use smi::{SmiError, collect};
