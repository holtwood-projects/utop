//! Pure NPU data model: no I/O, no `npu-smi`, no DCMI.
//!
//! `smi` turns command output into these types; `app` and `ui` only ever
//! read them. Keeping the model pure is what lets the parsers be tested
//! against fixtures without a live NPU.

use std::collections::BTreeMap;
use std::time::Instant;

/// Chip health as reported by `npu-smi`.
///
/// The tool can also emit a numeric alarm code instead of one of the named
/// states, so `NpuCard` keeps the raw string alongside this enum.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Health {
    #[default]
    Unknown,
    Ok,
    Warning,
    Alarm,
    Critical,
}

impl Health {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_uppercase().as_str() {
            "OK" => Self::Ok,
            "WARNING" | "WARN" => Self::Warning,
            "ALARM" => Self::Alarm,
            "CRITICAL" | "CRIT" | "FAULT" => Self::Critical,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "WARN",
            Self::Alarm => "ALARM",
            Self::Critical => "CRIT",
            Self::Unknown => "UNKNOWN",
        }
    }
}

/// One AI chip as shown in the `npu-smi info` device table.
#[derive(Clone, Debug, Default)]
pub struct NpuCard {
    /// NPU device id (`-i`), the user-visible card number.
    pub index: u32,
    /// Chip id inside the card; 0 is the AI chip, 1 is the MCU.
    pub chip: u32,
    pub name: String,
    pub health: Health,
    /// Raw health cell, so an alarm code is never silently dropped.
    pub health_raw: String,
    pub power_w: f32,
    pub temp_c: f32,
    pub hugepages_used: u64,
    pub hugepages_total: u64,
    pub aicore_pct: f32,
    /// DDR memory, used/total in MiB. 910B commonly reports 0/0.
    pub ddr_used_mb: u64,
    pub ddr_total_mb: u64,
    pub hbm_used_mb: u64,
    pub hbm_total_mb: u64,
    pub bus_id: String,
}

impl NpuCard {
    pub fn hbm_pct(&self) -> f64 {
        if self.hbm_total_mb == 0 {
            0.0
        } else {
            self.hbm_used_mb as f64 * 100.0 / self.hbm_total_mb as f64
        }
    }
}

/// One process entry from the `npu-smi info` process table.
///
/// A process pinned to several cards appears once per card; [`NpuProcRow`]
/// is the aggregated view used by the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NpuProcess {
    pub card: u32,
    pub chip: u32,
    pub pid: u32,
    pub name: String,
    pub hbm_mb: u64,
}

/// A process aggregated across every card it occupies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NpuProcRow {
    pub pid: u32,
    pub name: String,
    pub cards: Vec<u32>,
    pub hbm_mb: u64,
}

impl NpuProcRow {
    pub fn cards_label(&self) -> String {
        self.cards
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Everything one sampling round produced. Plain data, cloned across the
/// collector/UI boundary.
#[derive(Clone, Debug, Default)]
pub struct NpuSnapshot {
    pub tool_version: String,
    pub driver_version: String,
    pub collected_at: Option<Instant>,
    pub cards: Vec<NpuCard>,
    pub processes: Vec<NpuProcess>,
    /// Set when collection failed; the previous cards are kept by the UI so
    /// a transient failure does not blank the screen.
    pub error: Option<String>,
}

impl NpuSnapshot {
    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            error: Some(msg.into()),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    pub fn aggregate_processes(&self) -> Vec<NpuProcRow> {
        aggregate_processes(&self.processes)
    }
}

/// Collapses per-card process entries into one row per PID.
pub fn aggregate_processes(procs: &[NpuProcess]) -> Vec<NpuProcRow> {
    let mut map: BTreeMap<u32, NpuProcRow> = BTreeMap::new();
    for proc in procs {
        let row = map.entry(proc.pid).or_insert_with(|| NpuProcRow {
            pid: proc.pid,
            name: proc.name.clone(),
            cards: Vec::new(),
            hbm_mb: 0,
        });
        if !row.cards.contains(&proc.card) {
            row.cards.push(proc.card);
        }
        row.hbm_mb += proc.hbm_mb;
    }
    let mut rows: Vec<NpuProcRow> = map.into_values().collect();
    for row in &mut rows {
        row.cards.sort_unstable();
    }
    rows
}

/// HBM descending by default; PID ascending is the stable tiebreaker.
pub fn sort_proc_rows(rows: &mut [NpuProcRow], desc: bool) {
    rows.sort_by(|a, b| a.hbm_mb.cmp(&b.hbm_mb).then_with(|| a.pid.cmp(&b.pid)));
    if desc {
        rows.reverse();
    }
}

/// Case-insensitive match on process name or PID.
pub fn filter_proc_rows(rows: &[NpuProcRow], query: &str) -> Vec<NpuProcRow> {
    if query.is_empty() {
        return rows.to_vec();
    }
    let query = query.to_lowercase();
    rows.iter()
        .filter(|row| {
            row.name.to_lowercase().contains(&query) || row.pid.to_string().contains(&query)
        })
        .cloned()
        .collect()
}

/// HBM usage ratio, clamped for display; `0.0` when the total is unknown.
pub fn hbm_pct(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (used as f64 * 100.0 / total as f64).clamp(0.0, 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_(card: u32, pid: u32, name: &str, hbm_mb: u64) -> NpuProcess {
        NpuProcess {
            card,
            chip: 0,
            pid,
            name: name.to_string(),
            hbm_mb,
        }
    }

    #[test]
    fn health_parses_named_states_and_falls_back_to_unknown() {
        assert_eq!(Health::parse("OK"), Health::Ok);
        assert_eq!(Health::parse("warning"), Health::Warning);
        assert_eq!(Health::parse(" Alarm "), Health::Alarm);
        assert_eq!(Health::parse("Critical"), Health::Critical);
        assert_eq!(Health::parse("0x8F"), Health::Unknown);
    }

    #[test]
    fn aggregate_sums_hbm_and_dedups_cards() {
        let procs = vec![
            proc_(0, 100, "python", 10),
            proc_(1, 100, "python", 20),
            proc_(1, 200, "other", 5),
        ];
        let rows = aggregate_processes(&procs);
        assert_eq!(rows.len(), 2);
        let python = rows.iter().find(|r| r.pid == 100).unwrap();
        assert_eq!(python.hbm_mb, 30);
        assert_eq!(python.cards, vec![0, 1]);
        assert_eq!(python.cards_label(), "0,1");
    }

    #[test]
    fn sort_defaults_to_hbm_desc() {
        let mut rows = vec![
            NpuProcRow {
                pid: 2,
                hbm_mb: 10,
                ..Default::default()
            },
            NpuProcRow {
                pid: 1,
                hbm_mb: 30,
                ..Default::default()
            },
        ];
        sort_proc_rows(&mut rows, true);
        assert_eq!(rows[0].pid, 1);
        sort_proc_rows(&mut rows, false);
        assert_eq!(rows[0].pid, 2);
    }

    #[test]
    fn filter_matches_name_and_pid() {
        let rows = vec![
            NpuProcRow {
                pid: 1234,
                name: "Python".into(),
                ..Default::default()
            },
            NpuProcRow {
                pid: 9,
                name: "mindie".into(),
                ..Default::default()
            },
        ];
        assert_eq!(filter_proc_rows(&rows, "py").len(), 1);
        assert_eq!(filter_proc_rows(&rows, "123").len(), 1);
        assert_eq!(filter_proc_rows(&rows, "").len(), 2);
        assert_eq!(filter_proc_rows(&rows, "nope").len(), 0);
    }

    #[test]
    fn hbm_pct_handles_zero_total_and_clamps() {
        assert_eq!(hbm_pct(1, 0), 0.0);
        assert_eq!(hbm_pct(32, 64), 50.0);
        assert_eq!(hbm_pct(100, 64), 100.0);
    }

    #[test]
    fn card_hbm_pct_uses_its_own_total() {
        let card = NpuCard {
            hbm_used_mb: 16,
            hbm_total_mb: 64,
            ..Default::default()
        };
        assert_eq!(card.hbm_pct(), 25.0);
    }
}
