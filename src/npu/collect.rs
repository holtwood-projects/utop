//! Background NPU collector.
//!
//! `npu-smi` spawns a process and can take hundreds of milliseconds, so it
//! must never run on the rendering thread. The feed keeps exactly one
//! in-flight sample: a slow round is superseded by the next one instead of
//! queueing stale snapshots.
//!
//! One round costs at most four subprocess calls: `info` for the tables,
//! `usages` + `common` for the card the user has selected, and one `ecc`
//! call rotating through every card. Enrichment (`/proc`, `docker ps`)
//! runs on this same thread, so the event loop only ever clones data.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::model::{CardDetail, EccSample, NpuSnapshot};
use super::procfs::{self, Procfs};
use super::smi;

/// `docker ps` output is cached this long; container names change slowly
/// and the subprocess is only a best-effort decoration.
const DOCKER_CACHE_TTL: Duration = Duration::from_secs(30);
/// Sentinel for "no card selected": detail sampling is skipped.
pub const NO_CARD: u32 = u32::MAX;

/// Per-round NPU sampler. Owns the caches that must persist across
/// rounds: accumulated ECC counters, the `docker ps` map, and the
/// `/proc` reader. Used by the background feed and by `--once`.
pub struct NpuCollector {
    procfs: Procfs,
    /// (refreshed_at, container id → name); `docker ps` at most every TTL.
    docker: Option<(Instant, HashMap<String, String>)>,
    /// Latest ECC counters per card; one card is sampled per round.
    ecc: HashMap<u32, EccSample>,
    /// Next card to query for ECC, as an offset into the card list.
    ecc_cursor: usize,
}

impl Default for NpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl NpuCollector {
    pub fn new() -> Self {
        Self {
            procfs: Procfs::system(),
            docker: None,
            ecc: HashMap::new(),
            ecc_cursor: 0,
        }
    }

    /// Takes one full sample: the `info` tables, detail metrics for
    /// `selected` (skip on [`NO_CARD`]), one ECC card, and `/proc`
    /// enrichment for every process. A failed `info` returns the error
    /// snapshot immediately — the detail calls are skipped so a hung
    /// driver is reported after one timeout, not four.
    pub fn sample(&mut self, selected: u32) -> NpuSnapshot {
        let mut snapshot = match smi::collect() {
            Ok(snapshot) => snapshot,
            Err(err) => return NpuSnapshot::error(err.to_string()),
        };

        if snapshot.cards.iter().any(|card| card.index == selected) {
            snapshot.detail = Some(CardDetail {
                card: selected,
                usage: smi::collect_usages(selected).ok(),
                common: smi::collect_common(selected).ok(),
            });
        }

        // ECC sweeps one card per round; with ~1 s rounds an eight-card
        // machine is fully re-scanned every few seconds while each round
        // only pays for one extra subprocess.
        if !snapshot.cards.is_empty() {
            self.ecc
                .retain(|card, _| snapshot.cards.iter().any(|c| c.index == *card));
            self.ecc_cursor %= snapshot.cards.len();
            let card = snapshot.cards[self.ecc_cursor].index;
            if let Ok(ecc) = smi::collect_ecc(card) {
                self.ecc.insert(card, ecc);
            }
            self.ecc_cursor += 1;
            snapshot.ecc = self.ecc.clone();
        }

        if self
            .docker
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() > DOCKER_CACHE_TTL)
        {
            self.docker = Some((Instant::now(), procfs::docker_names()));
        }
        let containers = self
            .docker
            .as_ref()
            .map(|(_, map)| map)
            .filter(|m| !m.is_empty());
        snapshot.extras = self.procfs.enrich(&snapshot.processes, containers);

        snapshot
    }
}

/// Handle to the collector thread. Dropping it asks the thread to stop;
/// the thread is detached rather than joined so quitting never waits for a
/// hung `npu-smi` timeout.
pub struct NpuFeed {
    rx: Receiver<NpuSnapshot>,
    stop: Arc<AtomicBool>,
    _handle: JoinHandle<()>,
}

impl NpuFeed {
    /// Spawns the collector. `selected` carries the card index the detail
    /// calls should target; the UI writes it, the thread reads it.
    pub fn spawn(interval: Duration, selected: Arc<AtomicU32>) -> Self {
        let (tx, rx) = sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            let mut collector = NpuCollector::new();
            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }
                let snapshot = collector.sample(selected.load(Ordering::Relaxed));
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
