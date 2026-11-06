//! Terminal UI rendering with ratatui.
//!
//! Pure rendering: reads only the snapshots stored on `App` (process rows,
//! `SysStats`, `ProcDetails`) and never the live system. If you can build
//! an `App`, you can render and test every function in this module.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};

use crate::app::{App, InputMode, NpuFocus, View};
use crate::model::{ProcRow, SortKey, SysStats, display_name};
use crate::npu::model::{Health, NpuCard};

pub(crate) fn ui(frame: &mut ratatui::Frame<'_>, app: &App, table_state: &mut TableState) {
    match app.view {
        View::Npu => ui_npu(frame, app),
        View::System => ui_system(frame, app, table_state),
    }
}

fn ui_system(frame: &mut ratatui::Frame<'_>, app: &App, table_state: &mut TableState) {
    let summary = summary_content(app);
    // +2 for the block borders.
    let summary_height = summary.len() as u16 + 2;

    let mut constraints = vec![Constraint::Length(summary_height), Constraint::Min(5)];
    if app.show_details {
        constraints.push(Constraint::Length(7));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    draw_summary(frame, chunks[0], summary);
    draw_process_table(frame, chunks[1], app, table_state);
    if app.show_details {
        draw_process_details(frame, chunks[2], app);
    }
}

/// Width of the ASCII usage bars in characters.
const BAR_WIDTH: usize = 10;
/// At most this many rows of per-core meters (two cores per row).
const MAX_CORE_ROWS: usize = 4;

/// Green below 60%, yellow below 85%, red at or above.
fn load_color(pct: f64) -> Color {
    if pct < 60.0 {
        Color::Green
    } else if pct < 85.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Renders `[██████░░░░]  60.0%` for one meter.
fn usage_bar(pct: f32) -> String {
    let filled = ((pct as f64 / 100.0) * BAR_WIDTH as f64).round() as usize;
    let filled = filled.min(BAR_WIDTH);
    format!(
        "[{}{}] {:5.1}%",
        "\u{2588}".repeat(filled),
        "\u{2591}".repeat(BAR_WIDTH - filled),
        pct
    )
}

/// `3d 4h 12m` style uptime for the summary line.
fn format_uptime(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (hours, mins) = (rem / 3_600, rem % 3_600 / 60);
    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else {
        format!("{hours}h {mins}m")
    }
}

/// Builds the summary block: a stats line, a mode line with transient
/// status, a key-help line, and per-core CPU meters.
fn summary_content(app: &App) -> Vec<Line<'static>> {
    let mut lines = vec![stats_line(&app.stats), status_line(app), key_help_line()];
    lines.extend(per_core_meters(&app.stats.core_usages));
    lines
}

/// First summary line: title, CPU average, memory, load average, uptime.
fn stats_line(stats: &SysStats) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            " utop  ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "CPU {:.1}% ({} cores)  ",
                stats.cpu_avg,
                stats.core_usages.len()
            ),
            Style::default().fg(load_color(stats.cpu_avg)),
        ),
        Span::styled(
            format!(
                "Mem {:.2}/{:.2} GiB ({:.1}%)  ",
                stats.mem_used_gib, stats.mem_total_gib, stats.mem_pct
            ),
            Style::default().fg(load_color(stats.mem_pct)),
        ),
        Span::styled(
            format!(
                "Load {:.2} {:.2} {:.2}  ",
                stats.load_one, stats.load_five, stats.load_fifteen
            ),
            Style::default().fg(load_color(stats.load_pct)),
        ),
        Span::raw(format!("up {}", format_uptime(stats.uptime_secs))),
    ])
}

/// Second summary line: sort/view/filter/mode/paused indicators plus the
/// transient status message (if any).
fn status_line(app: &App) -> Line<'static> {
    let sort = match app.sort {
        SortKey::Cpu => "CPU",
        SortKey::Mem => "MEM",
        SortKey::Pid => "PID",
        SortKey::Name => "NAME",
    };
    let order = if app.desc { "desc" } else { "asc" };
    let mode = match app.mode {
        InputMode::Normal => "NORMAL",
        InputMode::Searching => "SEARCH",
        InputMode::ConfirmKill => "KILL?",
    };
    let view = if app.tree_mode { "TREE" } else { "LIST" };
    let filter_shown: String = if app.filter.is_empty() {
        "\u{2014}".into()
    } else {
        app.filter.clone()
    };
    let paused = if app.paused { "PAUSED" } else { "RUN" };

    let mut spans = vec![
        Span::raw(format!("Sort: {sort} ({order})  ")),
        Span::raw(format!("View: {view}  ")),
        Span::raw(format!("Filter: {filter_shown}  ")),
        Span::raw(format!("Mode: {mode}  ")),
        Span::raw(format!("Paused: {paused}")),
    ];
    if !app.status.is_empty() {
        spans.push(Span::styled(
            format!("  {}", app.status),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// Third summary line: a compact reminder of the key bindings.
fn key_help_line() -> Line<'static> {
    Line::from(
        "q quit | \u{2191}/\u{2193} move | s sort | r reverse | / search | t tree | p pause | k kill | d details",
    )
}

/// Per-core CPU meters, two cores per row, capped so huge machines stay
/// readable. Returns zero or more lines plus a trailing "N cores not shown"
/// line if any were elided.
fn per_core_meters(core_usages: &[f32]) -> Vec<Line<'static>> {
    let shown = core_usages.len().min(MAX_CORE_ROWS * 2);
    let indexed: Vec<(usize, f32)> = core_usages
        .iter()
        .enumerate()
        .take(shown)
        .map(|(index, usage)| (index, *usage))
        .collect();

    let mut lines = Vec::new();
    for pair in indexed.chunks(2) {
        let mut spans = Vec::new();
        for (index, pct) in pair {
            spans.push(Span::styled(
                format!(" {:>2} {}  ", index, usage_bar(*pct)),
                Style::default().fg(load_color(*pct as f64)),
            ));
        }
        lines.push(Line::from(spans));
    }
    if core_usages.len() > shown {
        lines.push(Line::from(format!(
            " +{} cores not shown",
            core_usages.len() - shown
        )));
    }
    lines
}

fn draw_summary(frame: &mut ratatui::Frame<'_>, area: Rect, lines: Vec<Line<'static>>) {
    let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Summary"));
    frame.render_widget(para, area);
}

/// Renders a `ProcRow` as a ratatui `Row`. Per-process CPU% is relative to
/// one core, so it can exceed 100% on multi-core machines; the thresholds
/// are scaled accordingly.
fn proc_row(row: &ProcRow) -> Row<'static> {
    let cpu_color = if row.cpu < 50.0 {
        Color::Green
    } else if row.cpu < 150.0 {
        Color::Yellow
    } else {
        Color::Red
    };
    Row::new(vec![
        Cell::from(row.pid.as_u32().to_string()),
        Cell::from(display_name(row)),
        Cell::from(Span::styled(
            format!("{:>6.1}", row.cpu),
            Style::default().fg(cpu_color),
        )),
        Cell::from(format!("{:>10}", row.mem_mb)),
    ])
}

fn draw_process_table(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    app: &App,
    table_state: &mut TableState,
) {
    let arrow = if app.desc { "\u{2193}" } else { "\u{2191}" };
    // Order must match `proc_row` and `widths`: PID | NAME | CPU% | MEM(MiB).
    let header_cells: Vec<String> = [
        (SortKey::Pid, "PID"),
        (SortKey::Name, "NAME"),
        (SortKey::Cpu, "CPU%"),
        (SortKey::Mem, "MEM(MiB)"),
    ]
    .into_iter()
    .map(|(key, label)| {
        if app.sort == key {
            format!("{label} {arrow}")
        } else {
            label.to_string()
        }
    })
    .collect();
    let header = Row::new(header_cells).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let rows = app.processes.iter().map(proc_row);

    let widths = [
        Constraint::Length(8),
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(12),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Processes ({})", app.processes.len())),
        )
        .row_highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
        .highlight_symbol("\u{25b6} ");

    frame.render_stateful_widget(table, area, table_state);
}

fn draw_process_details(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let Some(row) = app.processes.get(app.selected) else {
        let para = Paragraph::new("No process selected")
            .block(Block::default().borders(Borders::ALL).title("Details"));
        frame.render_widget(para, area);
        return;
    };

    // Live snapshot of the selected process; falls back to the row data
    // once the process is gone.
    let (name, ppid, status, cpu, mem_mib, exe, cmd) = match &app.details {
        Some(d) => (
            d.name.clone(),
            d.ppid
                .map(|p| p.as_u32().to_string())
                .unwrap_or_else(|| "\u{2014}".to_string()),
            d.status.clone(),
            format!("{:.1}", d.cpu),
            format!("{:.1}", d.mem_mib),
            d.exe.clone(),
            d.cmd.clone(),
        ),
        None => (
            row.name.clone(),
            row.ppid
                .map(|p| p.as_u32().to_string())
                .unwrap_or_else(|| "\u{2014}".to_string()),
            "Unknown".to_string(),
            format!("{:.1}", row.cpu),
            format!("{:.1}", row.mem_mb as f64),
            String::new(),
            String::new(),
        ),
    };

    let lines = vec![
        Line::from(vec![
            Span::styled(
                format!(" PID: {}", row.pid.as_u32()),
                Style::default().fg(Color::Yellow),
            ),
            Span::raw(format!("PPID: {ppid}  ")),
            Span::raw(format!("Status: {status}")),
        ]),
        Line::from(format!(" Name: {name}")),
        Line::from(format!(" CPU%: {cpu}  Mem: {mem_mib} MiB")),
        Line::from(format!(" Exe: {exe}")),
        Line::from(format!(" Cmd: {cmd}")),
    ];
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Details"));
    frame.render_widget(para, area);
}

// -- NPU view ------------------------------------------------------------

fn ui_npu(frame: &mut ratatui::Frame<'_>, app: &App) {
    let card_rows = app.npu.cards.len().clamp(1, 8) as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(card_rows + 2),
            Constraint::Length(6),
            Constraint::Min(5),
        ])
        .split(frame.area());

    let header = Paragraph::new(npu_header_lines(app))
        .block(Block::default().borders(Borders::ALL).title("utop-npu"));
    frame.render_widget(header, chunks[0]);
    draw_npu_cards(frame, chunks[1], app);
    draw_npu_details(frame, chunks[2], app);
    draw_npu_procs(frame, chunks[3], app);
}

fn npu_header_lines(app: &App) -> Vec<Line<'static>> {
    let npu = &app.npu;
    let driver = if npu.driver_version.is_empty() {
        "-"
    } else {
        &npu.driver_version
    };
    let tool = if npu.tool_version.is_empty() {
        "-"
    } else {
        &npu.tool_version
    };
    let mut first = vec![
        Span::styled(
            " utop-npu ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("driver {driver}  npu-smi {tool}  ")),
        Span::raw(if npu.collected_at.is_none() && npu.error.is_none() {
            format!("sampling...  {:.1}s", app.tick_rate_ms as f64 / 1000.0)
        } else {
            format!(
                "{} device(s)  {:.1}s",
                npu.cards.len(),
                app.tick_rate_ms as f64 / 1000.0
            )
        }),
    ];
    if app.paused {
        first.push(Span::styled(
            "  PAUSED",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(error) = &npu.error {
        first.push(Span::styled(
            format!("  {error}"),
            Style::default().fg(Color::Red),
        ));
    }

    let focus = match app.npu_focus {
        NpuFocus::Cards => "CARDS",
        NpuFocus::Procs => "PROCS",
    };
    let order = if app.npu_desc { "HBM desc" } else { "HBM asc" };
    let filter = if app.filter.is_empty() {
        "-".to_string()
    } else {
        app.filter.clone()
    };
    let mut second = vec![
        Span::raw(format!("Focus {focus}  Sort {order}  Filter {filter}  ")),
        Span::raw("Tab focus | F2 system | s sort | / filter | p pause | k kill | q quit"),
    ];
    if !app.status.is_empty() {
        second.push(Span::styled(
            format!("  {}", app.status),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    vec![Line::from(first), Line::from(second)]
}

fn draw_npu_cards(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let header = Row::new(["NPU", "NAME", "HEALTH", "TEMP", "POWER", "AICORE", "HBM"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.npu.cards.iter().map(card_row);
    let widths = [
        Constraint::Length(4),
        Constraint::Length(10),
        Constraint::Length(8),
        Constraint::Length(6),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Min(18),
    ];
    let mut state = TableState::default();
    state.select((!app.npu.cards.is_empty()).then_some(app.npu_card_selected));
    let title = if app.npu_focus == NpuFocus::Cards {
        format!("NPU devices * ({})", app.npu.cards.len())
    } else {
        format!("NPU devices ({})", app.npu.cards.len())
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
        .highlight_symbol("\u{25b6} ");
    frame.render_stateful_widget(table, area, &mut state);
}

fn card_row(card: &NpuCard) -> Row<'static> {
    Row::new(vec![
        Cell::from(card.index.to_string()),
        Cell::from(card.name.clone()),
        Cell::from(Span::styled(card.health.label(), health_style(card.health))),
        Cell::from(Span::styled(
            format!("{:.0}C", card.temp_c),
            Style::default().fg(temp_color(card.temp_c)),
        )),
        Cell::from(format!("{:.0}W", card.power_w)),
        Cell::from(Span::styled(
            format!("{:.0}%", card.aicore_pct),
            Style::default().fg(util_color(card.aicore_pct)),
        )),
        Cell::from(Span::styled(
            format!(
                "{:.1}/{:.0}G",
                card.hbm_used_mb as f64 / 1024.0,
                card.hbm_total_mb as f64 / 1024.0
            ),
            Style::default().fg(hbm_color(card.hbm_pct())),
        )),
    ])
}

fn draw_npu_details(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let lines = match app.selected_card() {
        Some(card) => vec![
            Line::from(vec![
                Span::styled(
                    format!(" NPU {}  ", card.index),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{}  ", card.name)),
                Span::styled(card.health.label().to_string(), health_style(card.health)),
                Span::raw(format!(
                    "  {:.0}C  {:.1}W  AICore {:.0}%",
                    card.temp_c, card.power_w, card.aicore_pct
                )),
            ]),
            Line::from(format!(
                " Bus {}  HBM {}/{} MB ({:.1}%)  Hugepages {}/{}",
                card.bus_id,
                card.hbm_used_mb,
                card.hbm_total_mb,
                card.hbm_pct(),
                card.hugepages_used,
                card.hugepages_total
            )),
            Line::from(format!(
                " DDR {}/{} MB  |  AIVector / AICPU / HBM BW: not sampled yet",
                card.ddr_used_mb, card.ddr_total_mb
            )),
        ],
        None => vec![Line::from(" No NPU detected")],
    };
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::ALL).title("Chip"));
    frame.render_widget(para, area);
}

fn draw_npu_procs(frame: &mut ratatui::Frame<'_>, area: Rect, app: &App) {
    let header = Row::new(["PID", "NAME", "NPU", "HBM(MB)"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.npu_procs.iter().map(|row| {
        Row::new(vec![
            Cell::from(row.pid.to_string()),
            Cell::from(row.name.clone()),
            Cell::from(row.cards_label()),
            Cell::from(format!("{:>10}", row.hbm_mb)),
        ])
    });
    let widths = [
        Constraint::Length(8),
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(12),
    ];
    let mut state = TableState::default();
    state.select((!app.npu_procs.is_empty()).then_some(app.npu_selected));
    let title = if app.npu_focus == NpuFocus::Procs {
        format!("NPU processes * ({})", app.npu_procs.len())
    } else {
        format!("NPU processes ({})", app.npu_procs.len())
    };
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .row_highlight_style(Style::default().bg(Color::Blue).fg(Color::White))
        .highlight_symbol("\u{25b6} ");
    frame.render_stateful_widget(table, area, &mut state);
}

fn health_style(health: Health) -> Style {
    let color = match health {
        Health::Ok => Color::Green,
        Health::Warning => Color::Yellow,
        Health::Alarm | Health::Critical => Color::Red,
        Health::Unknown => Color::DarkGray,
    };
    Style::default().fg(color)
}

/// 910B junction temperature: warn above 70C, critical above 80C.
fn temp_color(temp: f32) -> Color {
    if temp < 70.0 {
        Color::Green
    } else if temp <= 80.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

fn hbm_color(pct: f64) -> Color {
    if pct < 80.0 {
        Color::Green
    } else if pct < 90.0 {
        Color::Yellow
    } else {
        Color::Red
    }
}

/// Idle AICore is normal, so only "busy" is coloured; the absence of load
/// is shown grey rather than green/red.
fn util_color(pct: f32) -> Color {
    if pct < 1.0 {
        Color::DarkGray
    } else {
        Color::Green
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_bar_clamps_and_fills() {
        assert_eq!(
            usage_bar(0.0),
            "[\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}]   0.0%"
        );
        assert_eq!(
            usage_bar(100.0),
            "[\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}] 100.0%"
        );
        // Over 100% (per-core CPU can exceed 100): clamps at full.
        assert!(usage_bar(150.0).starts_with(
            "[\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}]"
        ));
        assert!(usage_bar(50.0).starts_with(
            "[\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2591}\u{2591}\u{2591}\u{2591}\u{2591}]"
        ));
    }

    #[test]
    fn format_uptime_variants() {
        assert_eq!(format_uptime(0), "0h 0m");
        assert_eq!(format_uptime(3_600), "1h 0m");
        assert_eq!(format_uptime(3_660), "1h 1m");
        assert_eq!(format_uptime(86_400), "1d 0h 0m");
        assert_eq!(format_uptime(2 * 86_400 + 3 * 3_600 + 4 * 60), "2d 3h 4m");
    }

    #[test]
    fn load_color_thresholds() {
        assert_eq!(load_color(0.0), Color::Green);
        assert_eq!(load_color(59.9), Color::Green);
        assert_eq!(load_color(60.0), Color::Yellow);
        assert_eq!(load_color(84.9), Color::Yellow);
        assert_eq!(load_color(85.0), Color::Red);
        assert_eq!(load_color(200.0), Color::Red);
    }

    #[test]
    fn npu_temperature_thresholds() {
        assert_eq!(temp_color(0.0), Color::Green);
        assert_eq!(temp_color(69.9), Color::Green);
        assert_eq!(temp_color(70.0), Color::Yellow);
        assert_eq!(temp_color(80.0), Color::Yellow);
        assert_eq!(temp_color(80.1), Color::Red);
    }

    #[test]
    fn npu_hbm_thresholds() {
        assert_eq!(hbm_color(0.0), Color::Green);
        assert_eq!(hbm_color(80.0), Color::Yellow);
        assert_eq!(hbm_color(90.0), Color::Red);
    }

    #[test]
    fn npu_idle_util_is_grey_not_red() {
        assert_eq!(util_color(0.0), Color::DarkGray);
        assert_eq!(util_color(1.0), Color::Green);
    }

    #[test]
    fn per_core_meters_caps_and_reports_elision() {
        // Zero cores: no lines at all.
        assert!(per_core_meters(&[]).is_empty());
        // 8 cores fit exactly in 4 rows of 2: no elision note.
        let lines = per_core_meters(&[0.0; 8]);
        assert_eq!(lines.len(), 4);
        // 9 cores: one extra "not shown" line.
        let lines = per_core_meters(&[0.0; 9]);
        assert_eq!(lines.len(), 5);
        let last = format!("{:?}", lines.last().unwrap());
        assert!(last.contains("not shown"));
    }
}
