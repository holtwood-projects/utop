//! Pure NPU data model: no I/O, no `npu-smi`, no DCMI.
//!
//! `smi` turns command output into these types; `app` and `ui` only ever
//! read them. Keeping the model pure is what lets the parsers be tested
//! against fixtures without a live NPU.

use std::collections::{BTreeMap, HashMap, VecDeque};
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
    /// Host-side PID, as `npu-smi` always reports it.
    pub pid: u32,
    /// The PID inside the process's own container, reported by `npu-smi`
    /// 26.x (`Process id in container` column); `None` on older tools.
    /// It resolves `/proc` only in that container's namespace — i.e. it
    /// works exactly when utop runs inside the same container.
    pub container_pid: Option<u32>,
    pub name: String,
    pub hbm_mb: u64,
}

/// Host-side facts about one NPU process, read from `/proc` by `procfs`.
///
/// `npu-smi` reports the *host* PID; inside a container without
/// `--pid=host` that PID has no `/proc` entry. On `npu-smi` 26.x the
/// process's *container* PID offers a second lookup path: it resolves
/// exactly when utop runs inside the same container (`via_container_pid`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcExtra {
    /// The PID resolves in the current PID namespace.
    pub visible: bool,
    /// The PID `/proc` was actually read from — the host PID on the host,
    /// or the container PID inside the process's own container.
    pub local_pid: Option<u32>,
    /// Set when `local_pid` is the *container* PID: enrichment succeeded
    /// because utop runs inside the same container as the process.
    pub via_container_pid: bool,
    /// Login name, or the numeric uid as a string when unresolvable.
    pub user: Option<String>,
    /// Resident host memory in MiB.
    pub rss_mb: Option<u64>,
    /// Full command line (`npu-smi` truncates the name at ~15 chars).
    pub cmdline: Option<String>,
    /// Container runtime id extracted from the cgroup path.
    pub container_id: Option<String>,
    /// Container name (docker) or the short id when only the id is known.
    pub container: Option<String>,
}

impl ProcExtra {
    /// Short label for the process-table CTN column.
    pub fn ctn_label(&self) -> &str {
        if let Some(container) = &self.container {
            container
        } else if !self.visible {
            "?"
        } else if self.via_container_pid {
            // `/proc` was reached through the container PID, so the
            // process lives in *this* container even when its cgroup
            // carries no runtime id (containers hide the host path).
            "local"
        } else {
            "host"
        }
    }
}

/// Rolling per-card history, kept on `App` for the detail-pane
/// sparklines: `npu-smi` only reports instant values, so trends need a
/// few minutes of rounds.
#[derive(Clone, Debug, Default)]
pub struct CardHist {
    /// AI Core utilisation percent, one point per sampling round.
    pub aicore: VecDeque<u64>,
    /// HBM fill percent, one point per sampling round.
    pub hbm: VecDeque<u64>,
}

/// About four minutes at the ~1 s collector cadence.
const HIST_CAP: usize = 240;

/// Pushes one round of readings into per-card ring buffers; histories of
/// cards that left the table are dropped.
pub fn update_card_history(hists: &mut HashMap<u32, CardHist>, cards: &[NpuCard]) {
    let alive: Vec<u32> = cards.iter().map(|c| c.index).collect();
    hists.retain(|card, _| alive.contains(card));
    for card in cards {
        let hist = hists.entry(card.index).or_default();
        push_cap(&mut hist.aicore, card.aicore_pct.round() as u64);
        push_cap(&mut hist.hbm, card.hbm_pct().round() as u64);
    }
}

fn push_cap(buf: &mut VecDeque<u64>, value: u64) {
    if buf.len() >= HIST_CAP {
        buf.pop_front();
    }
    buf.push_back(value);
}

/// A process aggregated across every card it occupies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NpuProcRow {
    pub pid: u32,
    pub name: String,
    pub cards: Vec<u32>,
    pub hbm_mb: u64,
    /// `/proc` enrichment; `None` when enrichment has not run.
    pub extra: Option<ProcExtra>,
}

impl NpuProcRow {
    pub fn cards_label(&self) -> String {
        self.cards
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// The pid a signal should target in *this* PID namespace: the
    /// container pid when `/proc` was reached through it, otherwise the
    /// host pid `npu-smi` reported (which will fail with ESRCH when it
    /// has no local `/proc` entry).
    pub fn signal_pid(&self) -> u32 {
        self.extra
            .as_ref()
            .and_then(|extra| extra.local_pid)
            .unwrap_or(self.pid)
    }
}

/// Parses `npu-smi info -t usages -i N` output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageSample {
    pub hbm_capacity_mb: u64,
    pub hbm_usage_pct: f32,
    pub aicore_pct: f32,
    pub aivector_pct: f32,
    pub aicpu_pct: f32,
    pub ctrlcpu_pct: f32,
    pub hbm_bw_pct: f32,
}

/// Parses `npu-smi info -t common -i N` output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommonSample {
    pub aicore_freq_mhz: u32,
    pub aicore_cur_freq_mhz: u32,
    pub aicore_count: u32,
    pub temp_c: f32,
    pub power_w: f32,
    pub mcu_temp_c: Option<f32>,
}

/// Parses `npu-smi info -t ecc -i N` output, summed over every reported
/// HBM block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EccSample {
    pub single_bit: u64,
    pub double_bit: u64,
    pub isolated_pages: u64,
}

/// Deeper metrics for one card, sampled on demand by `collect` for the
/// card the user is looking at (`npu-smi info -t` is one subprocess call
/// per query, so sampling every card every round would be too heavy).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardDetail {
    /// Card index (`npu-smi -i`) the sample belongs to; the UI ignores a
    /// detail whose card no longer matches the selection.
    pub card: u32,
    pub usage: Option<UsageSample>,
    pub common: Option<CommonSample>,
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
    /// Detail sample for the card selected when the round started.
    pub detail: Option<CardDetail>,
    /// Latest ECC counters per card, accumulated round-robin by the
    /// collector (one card per round keeps the subprocess count bounded).
    pub ecc: HashMap<u32, EccSample>,
    /// `/proc` enrichment per NPU PID; empty map until enrichment ran.
    pub extras: HashMap<u32, ProcExtra>,
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
        let mut rows = aggregate_processes(&self.processes);
        for row in &mut rows {
            row.extra = self.extras.get(&row.pid).cloned();
        }
        rows
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
            extra: None,
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

/// Parses `ASCEND_RT_VISIBLE_DEVICES` into the set of card indexes an
/// Ascend app would see. `npu-smi` always lists every card, so the env is
/// display context only: `None` (unset) means every card is visible, while
/// an explicitly empty value masks them all.
pub fn parse_visible_env(value: Option<&str>) -> Option<Vec<u32>> {
    let value = value?;
    Some(
        value
            .split(',')
            .filter_map(|token| token.trim().parse().ok())
            .collect(),
    )
}

/// A card is "hot but idle" once its HBM is nearly full while the AI Core
/// sits still — the signature of a dead worker still holding its memory.
pub const IDLE_HBM_PCT: f64 = 85.0;
/// AI Core readings at or below this count as idle.
pub const IDLE_AICORE_PCT: f32 = 2.0;
/// Consecutive hot-but-idle samples before the card is flagged. The
/// collector runs about once a second, so this is roughly five minutes —
/// long enough not to cry wolf on a legitimately idle loaded engine.
pub const IDLE_ROUNDS: u32 = 300;

/// Advances the per-card hot-but-idle streaks from one snapshot's cards.
/// Streaks for cards that left the table are dropped.
pub fn update_idle_streaks(streaks: &mut HashMap<u32, u32>, cards: &[NpuCard]) {
    let alive: Vec<u32> = cards.iter().map(|c| c.index).collect();
    streaks.retain(|card, _| alive.contains(card));
    for card in cards {
        if card.hbm_pct() >= IDLE_HBM_PCT && card.aicore_pct <= IDLE_AICORE_PCT {
            *streaks.entry(card.index).or_insert(0) += 1;
        } else {
            streaks.remove(&card.index);
        }
    }
}

/// One-line badge on a card row; at most one shows at a time, ordered by
/// severity so a sick card is never hidden behind an idle note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardFlag {
    /// Uncorrectable ECC errors or isolated HBM pages — check the hardware.
    EccAlarm,
    /// Correctable single-bit ECC errors observed.
    EccWarn,
    /// HBM nearly full with the AI Core idle for `IDLE_ROUNDS` samples.
    Idle,
    /// Not in `ASCEND_RT_VISIBLE_DEVICES`; invisible to NPU apps.
    Masked,
}

impl CardFlag {
    pub fn label(self) -> &'static str {
        match self {
            Self::EccAlarm => "ECC!",
            Self::EccWarn => "ecc",
            Self::Idle => "idle",
            Self::Masked => "masked",
        }
    }
}

/// Picks the badge for one card row, by severity.
pub fn card_flag(
    card: &NpuCard,
    ecc: Option<&EccSample>,
    idle_streak: u32,
    visible: Option<&[u32]>,
) -> Option<CardFlag> {
    if let Some(ecc) = ecc {
        if ecc.double_bit > 0 || ecc.isolated_pages > 0 {
            return Some(CardFlag::EccAlarm);
        }
        if ecc.single_bit > 0 {
            return Some(CardFlag::EccWarn);
        }
    }
    if idle_streak >= IDLE_ROUNDS {
        return Some(CardFlag::Idle);
    }
    if let Some(visible) = visible
        && !visible.contains(&card.index)
    {
        return Some(CardFlag::Masked);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_(card: u32, pid: u32, name: &str, hbm_mb: u64) -> NpuProcess {
        NpuProcess {
            card,
            chip: 0,
            pid,
            container_pid: None,
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

    #[test]
    fn parse_visible_env_distinguishes_unset_empty_and_list() {
        assert_eq!(parse_visible_env(None), None);
        assert_eq!(parse_visible_env(Some("")), Some(vec![]));
        assert_eq!(parse_visible_env(Some("0,2,3")), Some(vec![0, 2, 3]));
        // Whitespace tolerated, junk tokens skipped.
        assert_eq!(parse_visible_env(Some(" 4 , x ,7")), Some(vec![4, 7]));
    }

    fn hot_idle_card(index: u32) -> NpuCard {
        NpuCard {
            index,
            aicore_pct: 0.0,
            hbm_used_mb: 60_000,
            hbm_total_mb: 65_536,
            ..Default::default()
        }
    }

    #[test]
    fn idle_streak_counts_hot_idle_cards_and_resets() {
        let mut streaks = HashMap::new();
        update_idle_streaks(&mut streaks, &[hot_idle_card(3)]);
        update_idle_streaks(&mut streaks, &[hot_idle_card(3)]);
        assert_eq!(streaks[&3], 2);
        // Card back under load: streak clears.
        let mut busy = hot_idle_card(3);
        busy.aicore_pct = 50.0;
        update_idle_streaks(&mut streaks, &[busy]);
        assert!(!streaks.contains_key(&3));
        // Card vanished from the table: entry dropped.
        update_idle_streaks(&mut streaks, &[hot_idle_card(3)]);
        update_idle_streaks(&mut streaks, &[]);
        assert!(streaks.is_empty());
    }

    #[test]
    fn card_flag_prioritises_ecc_over_idle_over_masked() {
        let card = hot_idle_card(7);
        let ecc = EccSample {
            single_bit: 0,
            double_bit: 2,
            isolated_pages: 0,
        };
        assert_eq!(
            card_flag(&card, Some(&ecc), IDLE_ROUNDS, Some(&[0])),
            Some(CardFlag::EccAlarm)
        );
        let ecc = EccSample {
            single_bit: 5,
            ..Default::default()
        };
        assert_eq!(
            card_flag(&card, Some(&ecc), IDLE_ROUNDS, None),
            Some(CardFlag::EccWarn)
        );
        assert_eq!(
            card_flag(&card, None, IDLE_ROUNDS, Some(&[7])),
            Some(CardFlag::Idle)
        );
        assert_eq!(
            card_flag(&card, None, 0, Some(&[0, 1])),
            Some(CardFlag::Masked)
        );
        assert_eq!(card_flag(&card, None, 0, Some(&[7])), None);
        assert_eq!(card_flag(&card, None, 0, None), None);
    }

    #[test]
    fn aggregate_processes_method_attaches_extras() {
        let mut snapshot = NpuSnapshot {
            processes: vec![proc_(7, 22128, "VLLMEngineCor", 54256)],
            ..Default::default()
        };
        snapshot.extras.insert(
            22128,
            ProcExtra {
                visible: true,
                container: Some("pytorch".into()),
                ..Default::default()
            },
        );
        let rows = snapshot.aggregate_processes();
        assert_eq!(
            rows[0].extra.as_ref().and_then(|e| e.container.as_deref()),
            Some("pytorch")
        );
        // Free function keeps no extras (public API unchanged).
        let rows = aggregate_processes(&snapshot.processes);
        assert!(rows[0].extra.is_none());
    }

    #[test]
    fn signal_pid_prefers_local_pid_then_host_pid() {
        let mut row = NpuProcRow {
            pid: 4000000,
            ..Default::default()
        };
        assert_eq!(row.signal_pid(), 4000000);
        row.extra = Some(ProcExtra {
            visible: true,
            local_pid: Some(88),
            via_container_pid: true,
            ..Default::default()
        });
        assert_eq!(row.signal_pid(), 88);
    }

    #[test]
    fn card_history_caps_and_drops_vanished_cards() {
        let mut hists = HashMap::new();
        let card = |index: u32, aicore: f32, used: u64| NpuCard {
            index,
            aicore_pct: aicore,
            hbm_used_mb: used,
            hbm_total_mb: 100,
            ..Default::default()
        };
        update_card_history(&mut hists, &[card(0, 50.0, 60)]);
        update_card_history(&mut hists, &[card(0, 60.0, 70), card(1, 10.0, 20)]);
        let hist = &hists[&0];
        assert_eq!(
            hist.aicore.iter().copied().collect::<Vec<_>>(),
            vec![50, 60]
        );
        assert_eq!(hist.hbm.iter().copied().collect::<Vec<_>>(), vec![60, 70]);
        // Card 0 gone: its history is dropped, card 1 keeps growing.
        update_card_history(&mut hists, &[card(1, 20.0, 30)]);
        assert!(!hists.contains_key(&0));
        assert_eq!(hists[&1].aicore.len(), 2);
    }

    #[test]
    fn ctn_label_prefers_container_then_marks_invisible_then_host() {
        let extra = ProcExtra {
            visible: true,
            container: Some("mindie".into()),
            ..Default::default()
        };
        assert_eq!(extra.ctn_label(), "mindie");
        let extra = ProcExtra {
            visible: false,
            ..Default::default()
        };
        assert_eq!(extra.ctn_label(), "?");
        let extra = ProcExtra {
            visible: true,
            ..Default::default()
        };
        assert_eq!(extra.ctn_label(), "host");
        let extra = ProcExtra {
            visible: true,
            via_container_pid: true,
            ..Default::default()
        };
        assert_eq!(extra.ctn_label(), "local");
    }
}
