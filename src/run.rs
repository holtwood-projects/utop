//! The event loop: utop's impure shell.
//!
//! All sysinfo access lives in `collect`; this module drives it and owns
//! the terminal. Input handlers are pure: they mutate `App` and record the
//! collection work they need via [`Followup`], which [`settle`] performs —
//! so handlers are unit-testable without a live system.

use std::error::Error;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crossterm::{
    cursor::Show,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend, widgets::TableState};

use crate::app::{App, Followup, InputMode, View};
use crate::cli::Config;
use crate::collect::{self, KillOutcome, System};
use crate::model::{SignalKind, build_tree, resolve_selected_index, selected_pid};
use crate::npu::collect::{NO_CARD, NpuFeed};
use crate::ui::ui;

/// Lines moved per mouse-wheel scroll tick.
const SCROLL_STEP: usize = 3;
/// Lines moved per PageUp / PageDown.
const PAGE_STEP: usize = 10;

/// What a key handler wants the event loop to do after processing a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    /// Keep running the event loop.
    Continue,
    /// Exit the event loop cleanly (user pressed `q`).
    Quit,
}

/// Runs the TUI until the user quits. Takes the startup samples before
/// entering the alternate screen, so the ~220 ms gap sysinfo needs between
/// CPU samples is spent while the user's shell is still visible instead of
/// on a blank unresponsive screen.
pub fn run(config: &Config) -> Result<(), Box<dyn Error>> {
    install_panic_hook();
    let mut sys = collect::init_system();
    // One probe before taking over the terminal: it decides the initial
    // view and keeps the no-NPU case on the original process screen. The
    // shared `selected` slot lets the UI tell the collector which card to
    // run `npu-smi -t` detail queries on.
    let npu_selected = Arc::new(AtomicU32::new(NO_CARD));
    let npu_feed = crate::npu::smi::available().then(|| {
        let feed = NpuFeed::spawn(
            Duration::from_millis(config.delay_ms.max(1000)),
            Arc::clone(&npu_selected),
        );
        (feed, npu_selected)
    });

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // 初始化失败也必须恢复终端；否则 shell 会停在 raw mode + 备用屏幕。
    if let Err(err) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
        // execute! 可能已经写入备用屏幕序列；统一走 teardown，不能只关 raw mode。
        restore_terminal();
        return Err(err.into());
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(err) => {
            restore_terminal();
            return Err(err.into());
        }
    };

    let res = event_loop(&mut terminal, config, &mut sys, npu_feed);

    restore_terminal();
    res
}

/// Shared terminal teardown for the clean-exit and panic paths, so the
/// shell is never left in raw mode behind the alternate screen.
fn restore_terminal() {
    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture).ok();
    execute!(io::stdout(), Show).ok();
}

/// Restores the terminal if the app panics so utop never leaves the shell
/// stuck in raw mode behind the alternate screen.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original(info);
    }));
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    config: &Config,
    sys: &mut System,
    npu_feed: Option<(NpuFeed, Arc<AtomicU32>)>,
) -> Result<(), Box<dyn Error>> {
    let mut app = App::new(config);
    if npu_feed.is_some() {
        app.view = View::Npu;
        // `npu-smi` always lists every card; the env only decides which
        // ones an Ascend app can actually use.
        app.npu_visible = crate::npu::model::parse_visible_env(
            std::env::var("ASCEND_RT_VISIBLE_DEVICES").ok().as_deref(),
        );
    } else {
        app.set_status("npu-smi not found; showing system view".to_string());
    }
    collect::sync_app(sys, &mut app);

    let mut last_tick = Instant::now();
    let mut table_state = TableState::default();

    loop {
        // NPU samples arrive asynchronously; drain them every frame so a
        // slow tick rate does not delay the whole screen.
        if !app.paused
            && let Some((feed, _)) = &npu_feed
            && let Some(snapshot) = feed.poll()
        {
            app.apply_npu(snapshot);
        }
        // Publish the selected card so the collector's `-t` detail calls
        // track what the user is looking at.
        if let Some((_, selected)) = &npu_feed {
            selected.store(
                app.selected_card()
                    .map(|card| card.index)
                    .unwrap_or(NO_CARD),
                Ordering::Relaxed,
            );
        }

        let tick_rate = Duration::from_millis(app.tick_rate_ms);
        if last_tick.elapsed() >= tick_rate {
            if !app.paused {
                collect::do_refresh(sys, &mut app);
            }
            app.tick_status();
            last_tick = Instant::now();
        }

        table_state.select(if app.processes.is_empty() {
            None
        } else {
            Some(app.selected)
        });
        // Per frame, so the details panel tracks selection changes
        // immediately instead of waiting for the next refresh tick.
        collect::update_details(sys, &mut app);
        terminal.draw(|f| ui(f, &app, &mut table_state))?;

        // With an NPU feed, cap the event wait so samples are picked up
        // between ticks. Without one, keep the original idle behaviour.
        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        let timeout = if npu_feed.is_some() {
            timeout.min(Duration::from_millis(100))
        } else {
            timeout
        };
        if event::poll(timeout)? {
            match event::read()? {
                Event::Key(key) => {
                    if on_key(sys, &mut app, key) == Flow::Quit {
                        return Ok(());
                    }
                }
                Event::Mouse(mouse) => handle_mouse(&mut app, mouse),
                _ => {}
            }
        }
    }
}

/// Dispatches one key event, then performs the collection work (re-sort,
/// rebuild, refresh, kill) the handler requested. Returns `Flow::Quit` to
/// exit the loop.
fn on_key(sys: &mut System, app: &mut App, key: KeyEvent) -> Flow {
    if key.kind == KeyEventKind::Release {
        return Flow::Continue;
    }
    // In raw mode Ctrl+C arrives as a key event rather than SIGINT;
    // honor it as an exit request from any input mode.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Flow::Quit;
    }
    let flow = match app.mode {
        InputMode::Normal => handle_normal_key(app, key),
        InputMode::Searching => handle_search_key(app, key),
        InputMode::ConfirmKill => handle_kill_key(app, key),
    };
    if flow == Flow::Quit {
        return flow;
    }
    settle(sys, app);
    Flow::Continue
}

/// Performs the collection work requested by input handlers and executes an
/// armed kill. The only place outside `collect` that drives collection.
fn settle(sys: &mut System, app: &mut App) {
    // The filter is shared by both views, so re-apply it to NPU rows after
    // every input that may have changed it.
    app.reshape_npu_procs();
    match app.take_followup() {
        Followup::None => {}
        Followup::Reshape => resort(app),
        Followup::Rebuild => collect::rebuild_processes(sys, app),
        Followup::Refresh => collect::do_refresh(sys, app),
    }
    if let Some((pid, kind)) = app.take_kill_request() {
        match collect::kill(sys, pid, kind) {
            KillOutcome::Sent => {
                app.set_status(format!("sent {} to PID {}", kind.name(), pid.as_u32()));
                collect::do_refresh(sys, app);
            }
            KillOutcome::Failed => {
                app.set_status(format!("failed to kill PID {}", pid.as_u32()));
            }
            KillOutcome::Unsupported => {
                app.set_status(format!("{} unsupported on this platform", kind.name()));
            }
            KillOutcome::Gone => {
                // On the NPU view the PID may be host-side and simply
                // invisible in this container's PID namespace.
                let host_side = app
                    .npu_procs
                    .iter()
                    .find(|row| row.pid == pid.as_u32())
                    .and_then(|row| row.extra.as_ref())
                    .is_some_and(|extra| !extra.visible);
                if host_side {
                    app.set_status(format!(
                        "PID {} is host-side; not reachable from this namespace",
                        pid.as_u32()
                    ));
                } else {
                    app.set_status(format!("PID {} no longer exists", pid.as_u32()));
                }
            }
        }
    }
}

/// Re-applies ordering after the sort key or direction changes. Tree view
/// re-shapes the rows already collected (no new samples needed); list view
/// just re-sorts. Both preserve the selected PID.
fn resort(app: &mut App) {
    let preferred_pid = selected_pid(&app.processes, app.selected);
    let fallback_index = app.selected;
    if app.tree_active() {
        let rows = std::mem::take(&mut app.processes);
        app.processes = build_tree(rows, app.sort, app.desc, &app.collapsed);
        app.selected = resolve_selected_index(&app.processes, preferred_pid, fallback_index);
    } else {
        app.sort_processes_with_selection(preferred_pid, fallback_index);
    }
}

/// Mouse wheel scrolling of the process list.
fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => app.move_up(SCROLL_STEP),
        MouseEventKind::ScrollDown => app.move_down(SCROLL_STEP),
        _ => {}
    }
}

/// Normal-mode input: navigation, view toggles, sort, filter entry, kill.
/// Returns `Flow::Quit` if the user pressed `q`, `Flow::Continue` otherwise.
fn handle_normal_key(app: &mut App, key: KeyEvent) -> Flow {
    match key.code {
        KeyCode::Char('q') => return Flow::Quit,
        KeyCode::Up => app.move_up(1),
        KeyCode::Down => app.move_down(1),
        KeyCode::PageUp => app.move_up(PAGE_STEP),
        KeyCode::PageDown => app.move_down(PAGE_STEP),
        KeyCode::Home => app.select_first(),
        KeyCode::End => app.select_last(),
        KeyCode::Char('s') => {
            if app.view == View::Npu {
                app.toggle_npu_desc();
            } else {
                app.cycle_sort();
                app.request(Followup::Reshape);
            }
        }
        KeyCode::Char('r') => {
            if app.view == View::Npu {
                app.toggle_npu_desc();
            } else {
                app.toggle_desc();
                app.request(Followup::Reshape);
            }
        }
        KeyCode::Tab => {
            if app.view == View::Npu {
                app.toggle_npu_focus();
            }
        }
        KeyCode::F(2) => app.toggle_view(),
        KeyCode::Char('/') => app.enter_search(),
        KeyCode::Char('p') => app.toggle_paused(),
        KeyCode::Char('t') => {
            app.toggle_tree();
            app.request(Followup::Rebuild);
        }
        KeyCode::Char(' ') => {
            if app.toggle_collapse_selected().is_some() {
                app.request(Followup::Rebuild);
            }
        }
        KeyCode::F(5) => app.request(Followup::Refresh),
        KeyCode::Char('k') => {
            if let Some((pid, name)) = app.begin_kill_selected() {
                app.set_status(format!(
                    "Kill PID {} ({})? y = SIGTERM, K = SIGKILL, Esc = cancel",
                    pid.as_u32(),
                    name
                ));
            }
        }
        KeyCode::Char('-') => {
            app.adjust_tick_rate(-100);
            app.set_status(format!("refresh interval: {}ms", app.tick_rate_ms));
        }
        KeyCode::Char('+') | KeyCode::Char('=') => {
            app.adjust_tick_rate(100);
            app.set_status(format!("refresh interval: {}ms", app.tick_rate_ms));
        }
        KeyCode::Enter | KeyCode::Char('d') => app.toggle_details(),
        KeyCode::Esc if !app.filter.is_empty() => {
            app.clear_filter();
            app.request(Followup::Rebuild);
        }
        _ => {}
    }
    Flow::Continue
}

/// Search-mode input: append/delete filter chars, rebuild only on change.
/// Enter confirms without rebuilding — the list is already filtered.
fn handle_search_key(app: &mut App, key: KeyEvent) -> Flow {
    match key.code {
        KeyCode::Enter => app.enter_normal(),
        KeyCode::Esc => {
            app.clear_filter();
            app.enter_normal();
            app.request(Followup::Rebuild);
        }
        KeyCode::Backspace => {
            app.pop_filter();
            app.request(Followup::Rebuild);
        }
        KeyCode::Char(c) if !c.is_control() => {
            app.push_filter_char(c);
            app.request(Followup::Rebuild);
        }
        _ => {}
    }
    Flow::Continue
}

/// Kill-confirmation input: `y` = SIGTERM, `K` = SIGKILL, anything else cancels.
fn handle_kill_key(app: &mut App, key: KeyEvent) -> Flow {
    let kind = match key.code {
        KeyCode::Char('y') => Some(SignalKind::Term),
        KeyCode::Char('K') => Some(SignalKind::Kill),
        _ => None,
    };
    match kind {
        Some(kind) => {
            if !app.arm_kill(kind) {
                app.set_status("no kill in progress".to_string());
            }
            app.enter_normal();
        }
        None => {
            app.enter_normal();
            app.set_status("kill cancelled".to_string());
        }
    }
    Flow::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::testutil::row;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn key_with_kind(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind,
            state: KeyEventState::empty(),
        }
    }

    fn app_with_rows() -> App {
        let mut app = App::new(&Config::default());
        app.processes = vec![row(1, "alpha", 1.0, 10), row(2, "beta", 5.0, 20)];
        app
    }

    #[test]
    fn q_quits_from_normal_mode() {
        let mut app = app_with_rows();
        assert_eq!(
            handle_normal_key(&mut app, key(KeyCode::Char('q'))),
            Flow::Quit
        );
    }

    #[test]
    fn ctrl_c_quits_from_any_input_mode() {
        let mut app = app_with_rows();
        app.enter_search();
        let mut ctrl_c = key(KeyCode::Char('c'));
        ctrl_c.modifiers = KeyModifiers::CONTROL;
        assert_eq!(on_key(&mut System::new(), &mut app, ctrl_c), Flow::Quit);
    }

    #[test]
    fn release_events_are_ignored() {
        let mut app = app_with_rows();
        let release = key_with_kind(KeyCode::Char('q'), KeyEventKind::Release);
        assert_eq!(
            on_key(&mut System::new(), &mut app, release),
            Flow::Continue
        );
        // And the key was not processed.
        assert!(matches!(app.mode, InputMode::Normal));
    }

    #[test]
    fn sort_and_reverse_request_reshape() {
        let mut app = app_with_rows();
        handle_normal_key(&mut app, key(KeyCode::Char('s')));
        assert_eq!(app.take_followup(), Followup::Reshape);
        let before = app.desc;
        handle_normal_key(&mut app, key(KeyCode::Char('r')));
        assert_eq!(app.desc, !before);
        assert_eq!(app.take_followup(), Followup::Reshape);
    }

    #[test]
    fn tree_and_collapse_request_rebuild() {
        let mut app = app_with_rows();
        handle_normal_key(&mut app, key(KeyCode::Char('t')));
        assert!(app.tree_mode);
        assert_eq!(app.take_followup(), Followup::Rebuild);

        // Collapse toggle only fires in tree mode.
        app.selected = 0;
        handle_normal_key(&mut app, key(KeyCode::Char(' ')));
        assert_eq!(app.take_followup(), Followup::Rebuild);
    }

    #[test]
    fn f5_requests_refresh() {
        let mut app = app_with_rows();
        handle_normal_key(&mut app, key(KeyCode::F(5)));
        assert_eq!(app.take_followup(), Followup::Refresh);
    }

    #[test]
    fn search_flow_requests_rebuild_only_on_change() {
        let mut app = app_with_rows();
        handle_normal_key(&mut app, key(KeyCode::Char('/')));
        assert!(matches!(app.mode, InputMode::Searching));

        // Typing edits the filter and requests a rebuild.
        handle_search_key(&mut app, key(KeyCode::Char('b')));
        assert_eq!(app.filter, "b");
        assert_eq!(app.take_followup(), Followup::Rebuild);

        // Enter confirms without rebuilding: the list is already filtered.
        handle_search_key(&mut app, key(KeyCode::Enter));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.take_followup(), Followup::None);

        // Esc clears the filter and rebuilds.
        handle_normal_key(&mut app, key(KeyCode::Char('/')));
        handle_search_key(&mut app, key(KeyCode::Char('x')));
        handle_search_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.filter, "");
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.take_followup(), Followup::Rebuild);
    }

    #[test]
    fn kill_flow_arms_then_requests_execute() {
        let mut app = app_with_rows();
        app.selected = 0;
        handle_normal_key(&mut app, key(KeyCode::Char('k')));
        assert!(matches!(app.mode, InputMode::ConfirmKill));
        assert!(app.status.contains("Kill PID 1"));

        handle_kill_key(&mut app, key(KeyCode::Char('y')));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(
            app.take_kill_request(),
            Some((crate::model::Pid::from_u32(1), SignalKind::Term))
        );

        // Cancelling produces a status message and no request.
        handle_normal_key(&mut app, key(KeyCode::Char('k')));
        handle_kill_key(&mut app, key(KeyCode::Esc));
        assert_eq!(app.take_kill_request(), None);
        assert_eq!(app.status, "kill cancelled");
    }

    #[test]
    fn kill_arm_without_target_reports_instead_of_silence() {
        let mut app = app_with_rows();
        // No kill armed: any key in ConfirmKill mode just cancels cleanly.
        app.mode = InputMode::ConfirmKill;
        handle_kill_key(&mut app, key(KeyCode::Char('y')));
        assert!(matches!(app.mode, InputMode::Normal));
        assert_eq!(app.status, "no kill in progress");
        assert_eq!(app.take_kill_request(), None);
    }

    #[test]
    fn tick_rate_keys_adjust_within_bounds() {
        let mut app = app_with_rows();
        handle_normal_key(&mut app, key(KeyCode::Char('-')));
        assert_eq!(app.tick_rate_ms, 400);
        handle_normal_key(&mut app, key(KeyCode::Char('+')));
        handle_normal_key(&mut app, key(KeyCode::Char('=')));
        assert_eq!(app.tick_rate_ms, 600);
        // Clamps at the shared bounds.
        for _ in 0..100 {
            handle_normal_key(&mut app, key(KeyCode::Char('+')));
        }
        assert_eq!(app.tick_rate_ms, crate::cli::MAX_DELAY_MS);
    }

    #[test]
    fn resort_preserves_selection_by_pid_in_list_mode() {
        let mut app = App::new(&Config::default());
        app.processes = vec![
            row(1, "alpha", 1.0, 10),
            row(2, "beta", 5.0, 20),
            row(3, "gamma", 3.0, 30),
        ];
        app.selected = 2; // gamma
        resort(&mut app);
        // Default sort is CPU descending: beta, gamma, alpha.
        assert_eq!(app.processes[0].name, "beta");
        assert_eq!(app.processes[app.selected].name, "gamma");
    }

    #[test]
    fn resort_reshapes_tree_without_new_samples() {
        let mut app = App::new(&Config::default());
        let mut parent = row(1, "init", 0.0, 10);
        parent.ppid = None;
        let mut child = row(2, "bash", 1.0, 20);
        child.ppid = Some(crate::model::Pid::from_u32(1));
        app.processes = vec![parent, child];
        app.tree_mode = true;
        app.selected = 1; // child

        resort(&mut app);

        // Tree: init root, bash nested under it; selection follows the pid.
        assert_eq!(app.processes.len(), 2);
        assert!(app.processes[0].has_children);
        assert_eq!(app.processes[1].depth, 1);
        assert_eq!(app.processes[app.selected].pid.as_u32(), 2);
    }

    #[test]
    fn settle_executes_kill_and_refreshes() {
        // settle() with no pending work is a no-op that must not panic.
        let mut app = app_with_rows();
        settle(&mut System::new(), &mut app);
        assert_eq!(app.take_followup(), Followup::None);
        assert_eq!(app.take_kill_request(), None);
    }
}
