//! Fixture tests for `npu-smi` parsing. These run without an NPU, so the
//! text layer stays covered on CI machines that have no Ascend driver.

use utop::npu::model::Health;
use utop::npu::smi;

const INFO: &str = include_str!("fixtures/npu-smi-info-24.1.txt");
const IDLE: &str = include_str!("fixtures/npu-smi-info-idle.txt");
const INFO_25: &str = include_str!("fixtures/npu-smi-info-25.2.txt");
const USAGES_25: &str = include_str!("fixtures/npu-smi-usages-25.2.txt");
const ECC_25: &str = include_str!("fixtures/npu-smi-ecc-25.2.txt");

#[test]
fn parses_tool_and_driver_versions() {
    let parsed = smi::parse_info(INFO);
    assert_eq!(parsed.tool_version, "24.1.0.3");
    assert_eq!(parsed.driver_version, "24.1.0.3");
}

#[test]
fn parses_all_cards_in_order() {
    let parsed = smi::parse_info(INFO);
    assert_eq!(parsed.cards.len(), 8);
    assert_eq!(
        parsed.cards.iter().map(|c| c.index).collect::<Vec<_>>(),
        vec![0, 1, 2, 3, 4, 5, 6, 7]
    );
}

#[test]
fn parses_idle_card_fields() {
    let parsed = smi::parse_info(INFO);
    let card = &parsed.cards[0];
    assert_eq!(card.name, "910B3");
    assert_eq!(card.chip, 0);
    assert_eq!(card.health, Health::Ok);
    assert_eq!(card.power_w, 92.9);
    assert_eq!(card.temp_c, 37.0);
    assert_eq!(card.aicore_pct, 0.0);
    assert_eq!(card.hbm_used_mb, 3379);
    assert_eq!(card.hbm_total_mb, 65536);
    assert_eq!(card.bus_id, "0000:C1:00.0");
    assert_eq!((card.hugepages_used, card.hugepages_total), (0, 0));
}

#[test]
fn parses_warning_card_with_load() {
    let parsed = smi::parse_info(INFO);
    let card = &parsed.cards[2];
    assert_eq!(card.health, Health::Warning);
    assert_eq!(card.health_raw, "Warning");
    assert_eq!(card.temp_c, 83.0);
    assert_eq!(card.aicore_pct, 94.0);
    assert_eq!(card.hbm_used_mb, 61000);
    assert_eq!(card.hbm_total_mb, 65536);
}

#[test]
fn parses_process_table() {
    let parsed = smi::parse_info(INFO);
    assert_eq!(parsed.processes.len(), 3);
    let first = &parsed.processes[0];
    assert_eq!(first.card, 1);
    assert_eq!(first.chip, 0);
    assert_eq!(first.pid, 48122);
    assert_eq!(first.name, "python3");
    assert_eq!(first.hbm_mb, 18000);
}

#[test]
fn idle_process_table_yields_no_rows() {
    let parsed = smi::parse_info(IDLE);
    assert_eq!(parsed.cards.len(), 1);
    assert!(parsed.processes.is_empty());
}

#[test]
fn aggregates_processes_across_cards_without_merging_distinct_pids() {
    let parsed = smi::parse_info(INFO);
    let rows = utop::npu::model::aggregate_processes(&parsed.processes);
    assert_eq!(rows.len(), 3);
    // Two different PIDs share the name python3 and must stay separate.
    let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
    assert!(pids.contains(&48122));
    assert!(pids.contains(&51100));
    assert_eq!(rows.iter().find(|r| r.pid == 51100).unwrap().cards, vec![2]);
}

#[test]
fn json_output_is_well_formed_for_the_fixture() {
    let mut snapshot = utop::npu::model::NpuSnapshot::default();
    let parsed = smi::parse_info(INFO);
    snapshot.tool_version = parsed.tool_version;
    snapshot.driver_version = parsed.driver_version;
    snapshot.cards = parsed.cards;
    snapshot.processes = parsed.processes;
    let json = smi::to_json(&snapshot);
    assert!(json.starts_with("{\n"));
    assert!(json.trim_end().ends_with('}'));
    assert!(json.contains("\"index\": 2"));
    assert!(json.contains("\"health\": \"WARN\""));
    assert!(json.contains("\"pid\": 51100"));
    assert!(!json.contains("NaN"));
}

#[test]
fn parses_real_25_2_sample_with_non_contiguous_cards() {
    let parsed = smi::parse_info(INFO_25);
    assert_eq!(parsed.tool_version, "25.2.1");
    assert_eq!(parsed.driver_version, "25.2.1");
    assert_eq!(parsed.cards.len(), 3);
    assert_eq!(
        parsed.cards.iter().map(|c| c.index).collect::<Vec<_>>(),
        vec![0, 1, 7]
    );
    assert_eq!(parsed.cards[2].name, "910B2");
    // Regression: real output writes `57636/ 65536` without a space
    // before the slash; this used to parse as zero.
    assert_eq!(parsed.cards[2].hbm_used_mb, 57636);
    assert_eq!(parsed.cards[2].hbm_total_mb, 65536);
    assert_eq!(parsed.processes.len(), 1);
    assert_eq!(parsed.processes[0].pid, 22128);
    assert_eq!(parsed.processes[0].name, "VLLMEngineCor");
    assert_eq!(parsed.processes[0].hbm_mb, 54256);
}

#[test]
fn parses_real_usages_sample() {
    let sample = smi::parse_usages(USAGES_25);
    assert_eq!(sample.hbm_capacity_mb, 65536);
    assert_eq!(sample.hbm_usage_pct, 5.0);
    assert_eq!(sample.aicore_pct, 0.0);
    assert_eq!(sample.ctrlcpu_pct, 4.0);
    assert_eq!(sample.hbm_bw_pct, 0.0);
}

#[test]
fn parses_real_ecc_sample() {
    let sample = smi::parse_ecc(ECC_25);
    assert_eq!(sample.single_bit, 0);
    assert_eq!(sample.double_bit, 0);
    assert_eq!(sample.isolated_pages, 0);
}
