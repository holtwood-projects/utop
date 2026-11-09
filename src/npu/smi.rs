//! `npu-smi` invocation and parsing — the only module that spawns the tool.
//!
//! Two rules keep this layer honest:
//!
//! 1. Never parse the box-drawing characters; parse cells and whitespace
//!    separated numbers. `npu-smi` changes table widths between releases.
//! 2. Every subprocess has a deadline. A hung driver must not take down the
//!    collector thread and, through it, the whole TUI.

use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::model::{Health, NpuCard, NpuProcess, NpuSnapshot};

/// `npu-smi info` is the only call on the hot path; keep it bounded.
const INFO_TIMEOUT: Duration = Duration::from_secs(8);
/// Startup probe runs before raw mode; a hung driver must not block the TUI.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum SmiError {
    NotFound,
    Timeout,
    Spawn(io::Error),
    Failed { code: Option<i32>, stderr: String },
}

impl std::fmt::Display for SmiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "npu-smi not found (no Ascend driver?)"),
            Self::Timeout => write!(f, "npu-smi timed out"),
            Self::Spawn(err) => write!(f, "failed to run npu-smi: {err}"),
            Self::Failed { code, stderr } => {
                if stderr.is_empty() {
                    write!(f, "npu-smi failed (exit {code:?})")
                } else {
                    write!(f, "npu-smi failed (exit {code:?}): {stderr}")
                }
            }
        }
    }
}

impl std::error::Error for SmiError {}

/// Common driver library locations. Containers often install `npu-smi`
/// without an `ldconfig` entry, so an unset `LD_LIBRARY_PATH` makes it fail
/// on `libc_sec.so` (observed on a real 910B2 / npu-smi 25.2.1 host).
const DRIVER_LIB_DIRS: [&str; 3] = [
    "/usr/local/Ascend/driver/lib64/common",
    "/usr/local/Ascend/driver/lib64/driver",
    "/usr/local/dcmi",
];

fn command(args: &[&str]) -> Command {
    let mut cmd = Command::new("npu-smi");
    cmd.args(args);
    if std::env::var_os("LD_LIBRARY_PATH").is_none() {
        let dirs: Vec<&str> = DRIVER_LIB_DIRS
            .iter()
            .copied()
            .filter(|dir| Path::new(dir).is_dir())
            .collect();
        if !dirs.is_empty() {
            cmd.env("LD_LIBRARY_PATH", dirs.join(":"));
        }
    }
    cmd
}

/// True when the tool answers `-v` before [`PROBE_TIMEOUT`]. Does not prove a
/// device is present. Uses the same deadline loop as [`collect`] so a hung
/// driver cannot block startup before the terminal is initialised.
pub fn available() -> bool {
    run(&["-v"], PROBE_TIMEOUT).is_ok()
}

/// Takes one `npu-smi info` sample.
pub fn collect() -> Result<NpuSnapshot, SmiError> {
    let text = run(&["info"], INFO_TIMEOUT)?;
    let mut snapshot = parse_info(&text).into_snapshot();
    snapshot.collected_at = Some(Instant::now());
    Ok(snapshot)
}

fn run(args: &[&str], timeout: Duration) -> Result<String, SmiError> {
    let mut child = command(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                SmiError::NotFound
            } else {
                SmiError::Spawn(err)
            }
        })?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(SmiError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(SmiError::Spawn(err)),
        }
    }

    let output = child.wait_with_output().map_err(SmiError::Spawn)?;
    if !output.status.success() {
        return Err(SmiError::Failed {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Parsed form of a full `npu-smi info` sample.
#[derive(Debug, Default)]
pub struct ParsedInfo {
    pub tool_version: String,
    pub driver_version: String,
    pub cards: Vec<NpuCard>,
    pub processes: Vec<NpuProcess>,
}

impl ParsedInfo {
    pub fn into_snapshot(self) -> NpuSnapshot {
        NpuSnapshot {
            tool_version: self.tool_version,
            driver_version: self.driver_version,
            cards: self.cards,
            processes: self.processes,
            ..NpuSnapshot::default()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Device,
    Process,
}

#[derive(Default)]
struct DeviceHead {
    index: u32,
    name: String,
    health: Health,
    health_raw: String,
    power_w: f32,
    temp_c: f32,
    hugepages_used: u64,
    hugepages_total: u64,
}

impl DeviceHead {
    fn finish(self, tail: DeviceTail) -> NpuCard {
        NpuCard {
            index: self.index,
            chip: tail.chip,
            name: self.name,
            health: self.health,
            health_raw: self.health_raw,
            power_w: self.power_w,
            temp_c: self.temp_c,
            hugepages_used: self.hugepages_used,
            hugepages_total: self.hugepages_total,
            aicore_pct: tail.aicore_pct,
            ddr_used_mb: tail.ddr_used_mb,
            ddr_total_mb: tail.ddr_total_mb,
            hbm_used_mb: tail.hbm_used_mb,
            hbm_total_mb: tail.hbm_total_mb,
            bus_id: tail.bus_id,
        }
    }
}

#[derive(Default)]
struct DeviceTail {
    chip: u32,
    bus_id: String,
    aicore_pct: f32,
    ddr_used_mb: u64,
    ddr_total_mb: u64,
    hbm_used_mb: u64,
    hbm_total_mb: u64,
}

/// Parses the default `npu-smi info` output: device table plus process table.
///
/// The device table is two lines per chip. The second line is recognised by
/// a bus id (`0000:..`) in the Health column, so a malformed first line can
/// never silently swallow the next chip.
pub fn parse_info(text: &str) -> ParsedInfo {
    let mut parsed = ParsedInfo::default();
    let mut section = Section::None;
    let mut head: Option<DeviceHead> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(version) = line.split("npu-smi ").nth(1) {
            parsed.tool_version = version
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
        }
        if let Some((_, rest)) = line.split_once("Version:") {
            parsed.driver_version = rest.trim().trim_end_matches('|').trim().to_string();
        }

        if line.contains("Process id") {
            flush_head(&mut parsed, &mut head);
            section = Section::Process;
            continue;
        }
        if line.contains("NPU") && line.contains("Name") && line.contains("Health") {
            flush_head(&mut parsed, &mut head);
            section = Section::Device;
            continue;
        }
        if !line.starts_with('|') {
            continue;
        }

        let cells = split_cells(line);
        match section {
            Section::Device => {
                if cells.len() < 3 {
                    continue;
                }
                if cells[1].contains(':') {
                    if let Some(head) = head.take() {
                        parsed.cards.push(head.finish(parse_device_tail(&cells)));
                    }
                } else if let Some(parsed_head) = parse_device_head(&cells) {
                    flush_head(&mut parsed, &mut head);
                    head = Some(parsed_head);
                }
            }
            Section::Process => {
                if let Some(process) = parse_process(&cells) {
                    parsed.processes.push(process);
                }
            }
            Section::None => {}
        }
    }
    flush_head(&mut parsed, &mut head);
    parsed
}

fn flush_head(parsed: &mut ParsedInfo, head: &mut Option<DeviceHead>) {
    if let Some(head) = head.take() {
        parsed.cards.push(head.finish(DeviceTail::default()));
    }
}

fn split_cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn parse_device_head(cells: &[&str]) -> Option<DeviceHead> {
    let mut parts = cells[0].split_whitespace();
    let index = parts.next()?.parse().ok()?;
    let name = parts.collect::<Vec<_>>().join(" ");
    let health_raw = cells[1].to_string();
    let tokens: Vec<&str> = cells[2].split_whitespace().collect();
    let (power_tok, temp_tok, rest) = match tokens.as_slice() {
        [power, temp, rest @ ..] => (*power, *temp, rest),
        _ => return None,
    };
    let power_w = power_tok.parse().unwrap_or(0.0);
    let temp_c = temp_tok.parse().unwrap_or(0.0);
    let (hugepages_used, hugepages_total) = number_pairs(rest).first().copied().unwrap_or((0, 0));
    Some(DeviceHead {
        index,
        name,
        health: Health::parse(&health_raw),
        health_raw,
        power_w,
        temp_c,
        hugepages_used,
        hugepages_total,
    })
}

fn parse_device_tail(cells: &[&str]) -> DeviceTail {
    let chip = cells[0].parse().unwrap_or(0);
    let bus_id = cells[1].to_string();
    let tokens: Vec<&str> = cells[2].split_whitespace().collect();
    let (aicore_tok, rest) = tokens.split_first().unwrap_or((&"0", &[]));
    let pairs = number_pairs(rest);
    let (ddr_used_mb, ddr_total_mb) = if pairs.len() >= 2 { pairs[0] } else { (0, 0) };
    let (hbm_used_mb, hbm_total_mb) = pairs.last().copied().unwrap_or((0, 0));
    DeviceTail {
        chip,
        bus_id,
        aicore_pct: aicore_tok.parse().unwrap_or(0.0),
        ddr_used_mb,
        ddr_total_mb,
        hbm_used_mb,
        hbm_total_mb,
    }
}

/// Scans for `a / b` number pairs. `npu-smi` writes used/total cells as
/// `3379 / 65536`; everything else is ignored.
fn number_pairs(tokens: &[&str]) -> Vec<(u64, u64)> {
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        // Real 25.x output writes `57636/ 65536` without a space before
        // the slash, while older releases write `3379 / 65536`.
        if let Some((left, right)) = token.split_once('/') {
            if !right.is_empty() {
                if let (Ok(used), Ok(total)) = (left.parse(), right.parse()) {
                    pairs.push((used, total));
                }
                i += 1;
                continue;
            }
            if i + 1 < tokens.len()
                && let (Ok(used), Ok(total)) = (left.parse(), tokens[i + 1].parse())
            {
                pairs.push((used, total));
                i += 2;
                continue;
            }
        }
        if i + 2 < tokens.len()
            && tokens[i + 1] == "/"
            && let (Ok(used), Ok(total)) = (tokens[i].parse(), tokens[i + 2].parse())
        {
            pairs.push((used, total));
            i += 3;
            continue;
        }
        i += 1;
    }
    pairs
}

fn parse_process(cells: &[&str]) -> Option<NpuProcess> {
    if cells.len() < 4 {
        return None;
    }
    let mut ids = cells[0].split_whitespace();
    let card = ids.next()?.parse().ok()?;
    let chip = ids.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let pid = cells[1].parse().ok()?;
    let name = cells[2].to_string();
    let hbm_mb = cells[3].parse().unwrap_or(0);
    Some(NpuProcess {
        card,
        chip,
        pid,
        name,
        hbm_mb,
    })
}

/// Normalises a `key : value` key so lookups survive spacing and unit
/// changes: `Aicore Usage Rate(%)` becomes `aicore_usage_rate`.
pub fn norm_key(key: &str) -> String {
    let mut out = String::new();
    let mut pending_sep = false;
    let mut in_units = false;
    for ch in key.chars() {
        match ch {
            '(' => {
                in_units = true;
                pending_sep = true;
            }
            ')' => {
                in_units = false;
                pending_sep = true;
            }
            _ if in_units => {}
            ch if ch.is_ascii_alphanumeric() => {
                if pending_sep && !out.is_empty() {
                    out.push('_');
                }
                out.push(ch.to_ascii_lowercase());
                pending_sep = false;
            }
            _ => pending_sep = true,
        }
    }
    out
}

/// Parses `-t` style output into normalised key/value pairs.
pub fn parse_kv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let key = norm_key(key);
            if key.is_empty() {
                return None;
            }
            Some((key, value.trim().to_string()))
        })
        .collect()
}

fn kv_value<'a>(kv: &'a [(String, String)], key: &str) -> Option<&'a str> {
    kv.iter()
        .find(|(k, _)| k == key)
        .map(|(_, value)| value.as_str())
}

fn kv_f32(kv: &[(String, String)], key: &str) -> Option<f32> {
    kv_value(kv, key)?.trim_end_matches('%').trim().parse().ok()
}

fn kv_u64(kv: &[(String, String)], key: &str) -> Option<u64> {
    kv_f32(kv, key).map(|value| value.max(0.0) as u64)
}

/// Parses `npu-smi info -t usages -i N`.
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

pub fn parse_usages(text: &str) -> UsageSample {
    let kv = parse_kv(text);
    UsageSample {
        hbm_capacity_mb: kv_u64(&kv, "hbm_capacity").unwrap_or(0),
        hbm_usage_pct: kv_f32(&kv, "hbm_usage_rate").unwrap_or(0.0),
        aicore_pct: kv_f32(&kv, "aicore_usage_rate").unwrap_or(0.0),
        aivector_pct: kv_f32(&kv, "aivector_usage_rate").unwrap_or(0.0),
        aicpu_pct: kv_f32(&kv, "aicpu_usage_rate").unwrap_or(0.0),
        ctrlcpu_pct: kv_f32(&kv, "ctrlcpu_usage_rate").unwrap_or(0.0),
        hbm_bw_pct: kv_f32(&kv, "hbm_bandwidth_usage_rate").unwrap_or(0.0),
    }
}

/// Parses `npu-smi info -t common -i N`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CommonSample {
    pub aicore_freq_mhz: u32,
    pub aicore_cur_freq_mhz: u32,
    pub aicore_count: u32,
    pub temp_c: f32,
    pub power_w: f32,
    pub mcu_temp_c: Option<f32>,
}

pub fn parse_common(text: &str) -> CommonSample {
    let kv = parse_kv(text);
    // The MCU block reports a second `Temperature(C)`; `parse_kv` keeps the
    // first occurrence for each normalised key, so read the raw lines for
    // the MCU value.
    let mcu_temp_c = text
        .lines()
        .skip_while(|line| !line.contains("Chip Name") || !line.contains("mcu"))
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            (norm_key(key) == "temperature").then(|| value.trim().parse().ok())?
        });
    CommonSample {
        aicore_freq_mhz: kv_u64(&kv, "aicore_freq").unwrap_or(0) as u32,
        aicore_cur_freq_mhz: kv_u64(&kv, "aicore_curfreq").unwrap_or(0) as u32,
        aicore_count: kv_u64(&kv, "aicore_count").unwrap_or(0) as u32,
        temp_c: kv_f32(&kv, "temperature").unwrap_or(0.0),
        power_w: kv_f32(&kv, "npu_real_time_power").unwrap_or(0.0),
        mcu_temp_c,
    }
}

/// Parses `npu-smi info -t ecc -i N`, summing every reported HBM block.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EccSample {
    pub single_bit: u64,
    pub double_bit: u64,
    pub isolated_pages: u64,
}

pub fn parse_ecc(text: &str) -> EccSample {
    let mut sample = EccSample::default();
    for (key, value) in parse_kv(text) {
        let Ok(value) = value.parse::<u64>() else {
            continue;
        };
        match key.as_str() {
            "hbm_single_bit_error_count" => sample.single_bit += value,
            "hbm_double_bit_error_count" => sample.double_bit += value,
            "hbm_single_bit_isolated_pages_count"
            | "hbm_double_bit_isolated_pages_count"
            | "isolated_pages_count" => sample.isolated_pages += value,
            _ => {}
        }
    }
    sample
}

/// Renders a snapshot as a plain table for `--once`.
pub fn render_text(snapshot: &NpuSnapshot) -> String {
    let mut out = String::new();
    let tool = if snapshot.tool_version.is_empty() {
        "npu-smi"
    } else {
        &snapshot.tool_version
    };
    out.push_str(&format!(
        "utop-npu  {tool}  driver {}  {} device(s)\n",
        if snapshot.driver_version.is_empty() {
            "-"
        } else {
            &snapshot.driver_version
        },
        snapshot.cards.len()
    ));
    if let Some(error) = &snapshot.error {
        out.push_str(&format!("error: {error}\n"));
    }
    if snapshot.cards.is_empty() {
        return out;
    }

    out.push_str(&format!(
        "{:<4} {:<10} {:<8} {:>6} {:>8} {:>8} {:>18}\n",
        "NPU", "NAME", "HEALTH", "TEMP", "POWER", "AICORE", "HBM"
    ));
    for card in &snapshot.cards {
        out.push_str(&format!(
            "{:<4} {:<10} {:<8} {:>5.0}C {:>7.1}W {:>7.0}% {:>9}/{:<8}\n",
            card.index,
            card.name,
            card.health.label(),
            card.temp_c,
            card.power_w,
            card.aicore_pct,
            card.hbm_used_mb,
            format!("{}MB", card.hbm_total_mb),
        ));
    }

    let mut rows = snapshot.aggregate_processes();
    super::model::sort_proc_rows(&mut rows, true);
    if rows.is_empty() {
        out.push_str("\nno NPU processes\n");
        return out;
    }
    out.push_str(&format!(
        "\n{:<8} {:<24} {:<8} {:>12}\n",
        "PID", "NAME", "NPU", "HBM(MB)"
    ));
    for row in rows {
        out.push_str(&format!(
            "{:<8} {:<24} {:<8} {:>12}\n",
            row.pid,
            row.name,
            row.cards_label(),
            row.hbm_mb
        ));
    }
    out
}

/// Minimal JSON writer, so the project keeps its three-crate dependency set.
pub fn to_json(snapshot: &NpuSnapshot) -> String {
    let mut out = String::from("{\n");
    out.push_str(&format!(
        "  \"tool_version\": \"{}\",\n  \"driver_version\": \"{}\",\n",
        escape_json(&snapshot.tool_version),
        escape_json(&snapshot.driver_version)
    ));
    match &snapshot.error {
        Some(error) => out.push_str(&format!("  \"error\": \"{}\",\n", escape_json(error))),
        None => out.push_str("  \"error\": null,\n"),
    }
    out.push_str("  \"cards\": [\n");
    for (index, card) in snapshot.cards.iter().enumerate() {
        let comma = if index + 1 == snapshot.cards.len() {
            ""
        } else {
            ","
        };
        out.push_str(&format!(
            "    {{\"index\": {}, \"chip\": {}, \"name\": \"{}\", \"health\": \"{}\", \
             \"health_raw\": \"{}\", \"power_w\": {}, \"temp_c\": {}, \"aicore_pct\": {}, \
             \"hbm_used_mb\": {}, \"hbm_total_mb\": {}, \"bus_id\": \"{}\"}}{}\n",
            card.index,
            card.chip,
            escape_json(&card.name),
            card.health.label(),
            escape_json(&card.health_raw),
            json_num(card.power_w),
            json_num(card.temp_c),
            json_num(card.aicore_pct),
            card.hbm_used_mb,
            card.hbm_total_mb,
            escape_json(&card.bus_id),
            comma
        ));
    }
    out.push_str("  ],\n  \"processes\": [\n");
    for (index, process) in snapshot.processes.iter().enumerate() {
        let comma = if index + 1 == snapshot.processes.len() {
            ""
        } else {
            ","
        };
        out.push_str(&format!(
            "    {{\"card\": {}, \"chip\": {}, \"pid\": {}, \"name\": \"{}\", \"hbm_mb\": {}}}{}\n",
            process.card,
            process.chip,
            process.pid,
            escape_json(&process.name),
            process.hbm_mb,
            comma
        ));
    }
    out.push_str("  ]\n}\n");
    out
}

fn json_num(value: f32) -> String {
    if value.is_finite() {
        format!("{value:.1}")
    } else {
        "0.0".to_string()
    }
}

fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_key_drops_units_and_spacing() {
        assert_eq!(norm_key("Aicore Usage Rate(%)"), "aicore_usage_rate");
        assert_eq!(norm_key("HBM Capacity(MB)"), "hbm_capacity");
        assert_eq!(norm_key("NPU Real-time Power(W)"), "npu_real_time_power");
        assert_eq!(norm_key("  Aicore curFreq(MHZ)  "), "aicore_curfreq");
    }

    #[test]
    fn number_pairs_reads_used_total_cells() {
        let tokens = ["0", "/", "0", "3379", "/", "65536"];
        assert_eq!(number_pairs(&tokens), vec![(0, 0), (3379, 65536)]);
        assert!(number_pairs(&["0", "1", "2"]).is_empty());
    }

    #[test]
    fn number_pairs_accepts_tight_slash_from_25_x() {
        assert_eq!(number_pairs(&["57636/", "65536"]), vec![(57636, 65536)]);
        assert_eq!(number_pairs(&["57636/65536"]), vec![(57636, 65536)]);
        assert_eq!(number_pairs(&["0", "/", "0"]), vec![(0, 0)]);
    }

    #[test]
    fn parse_ecc_matches_real_isolated_page_keys() {
        let text = "\
HBM Single Bit Isolated Pages Count : 2
HBM Double Bit Isolated Pages Count : 1
";
        assert_eq!(parse_ecc(text).isolated_pages, 3);
    }

    #[test]
    fn parse_usages_maps_known_fields() {
        let text = "\
NPU ID                         : 7
Chip ID                        : 0
HBM Capacity(MB)               : 65536
HBM Usage Rate(%)              : 5
Aicore Usage Rate(%)           : 61
Aivector Usage Rate(%)         : 58
Aicpu Usage Rate(%)            : 2
Ctrlcpu Usage Rate(%)          : 1
HBM Bandwidth Usage Rate(%)    : 42
";
        let sample = parse_usages(text);
        assert_eq!(sample.hbm_capacity_mb, 65536);
        assert_eq!(sample.hbm_usage_pct, 5.0);
        assert_eq!(sample.aicore_pct, 61.0);
        assert_eq!(sample.hbm_bw_pct, 42.0);
    }

    #[test]
    fn parse_common_reads_frequency_and_mcu_temperature() {
        let text = "\
Chip ID                        : 0
Aicore Count                   : 24
Aicore Freq(MHZ)               : 1800
Aicore curFreq(MHZ)            : 800
Temperature(C)                 : 67
NPU Real-time Power(W)         : 296.5
Chip Name                      : mcu
Temperature(C)                 : 56
";
        let sample = parse_common(text);
        assert_eq!(sample.aicore_count, 24);
        assert_eq!(sample.aicore_freq_mhz, 1800);
        assert_eq!(sample.aicore_cur_freq_mhz, 800);
        assert_eq!(sample.temp_c, 67.0);
        assert_eq!(sample.power_w, 296.5);
        assert_eq!(sample.mcu_temp_c, Some(56.0));
    }

    #[test]
    fn parse_ecc_sums_blocks() {
        let text = "\
HBM Single Bit Error Count     : 3
HBM Double Bit Error Count     : 0
Isolated Pages Count           : 1
HBM Single Bit Error Count     : 4
HBM Double Bit Error Count     : 1
Isolated Pages Count           : 0
";
        let sample = parse_ecc(text);
        assert_eq!(sample.single_bit, 7);
        assert_eq!(sample.double_bit, 1);
        assert_eq!(sample.isolated_pages, 1);
    }

    #[test]
    fn json_escapes_control_characters() {
        assert_eq!(escape_json("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
