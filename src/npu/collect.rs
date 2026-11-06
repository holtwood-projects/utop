//! Background NPU collector.
//!
//! `npu-smi` spawns a process and can take hundreds of milliseconds, so it
//! must never run on the rendering thread. The feed keeps exactly one
//! in-flight sample: a slow round is superseded by the next one instead of
//! queueing stale snapshots.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::model::NpuSnapshot;
use super::smi;

/// Handle to the collector thread. Dropping it asks the thread to stop;
/// the thread is detached rather than joined so quitting never waits for a
/// hung `npu-smi` timeout.
pub struct NpuFeed {
    rx: Receiver<NpuSnapshot>,
    stop: Arc<AtomicBool>,
    _handle: JoinHandle<()>,
}

impl NpuFeed {
    pub fn spawn(interval: Duration) -> Self {
        let (tx, rx) = sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let snapshot = match smi::collect() {
                    Ok(snapshot) => snapshot,
                    Err(err) => NpuSnapshot::error(err.to_string()),
                };
                // Full channel means the UI has not consumed the previous
                // sample yet; keep the newer one and let the next round
                // replace it.
                let _ = tx.try_send(snapshot);
                if !sleep_interruptible(&stop_flag, interval) {
                    break;
                }
            }
        });
        Self {
            rx,
            stop,
            _handle: handle,
        }
    }

    /// Returns only the newest sample; older queued samples are discarded.
    pub fn poll(&self) -> Option<NpuSnapshot> {
        let mut latest = None;
        while let Ok(snapshot) = self.rx.try_recv() {
            latest = Some(snapshot);
        }
        latest
    }
}

impl Drop for NpuFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn sleep_interruptible(stop: &AtomicBool, total: Duration) -> bool {
    const STEP: Duration = Duration::from_millis(100);
    let mut left = total;
    while left > Duration::ZERO {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let chunk = left.min(STEP);
        thread::sleep(chunk);
        left -= chunk;
    }
    !stop.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_sleep_returns_false() {
        let stop = AtomicBool::new(true);
        assert!(!sleep_interruptible(&stop, Duration::from_millis(500)));
    }

    #[test]
    fn normal_sleep_returns_true() {
        let stop = AtomicBool::new(false);
        assert!(sleep_interruptible(&stop, Duration::from_millis(1)));
    }
}
