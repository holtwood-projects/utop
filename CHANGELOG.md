# Changelog

All notable changes to this project are documented in this file.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Ascend NPU view (`F2`), sampling `npu-smi info` on a background thread:
  per-card health, temperature, power, AI Core utilisation and HBM usage,
  plus the NPU process table aggregated by PID. `Tab` switches focus
  between the card table and the process table.
- Per-card `npu-smi -t` detail for the *selected* card — AIVector / AICPU /
  Ctrlcpu rates, HBM bandwidth, AI Core frequency, core count and MCU
  temperature — filling the metrics pane that previously said
  "not sampled yet".
- ECC counters swept one card per round, and a `NOTE` column on the card
  table: `ECC!` (double-bit errors or isolated pages), `ecc` (single-bit),
  `idle` (HBM ≥ 85 % with AI Core ≤ 2 % for ~5 minutes — the dead-worker
  signature) and `masked` (outside `ASCEND_RT_VISIBLE_DEVICES`, echoed in
  the header when set).
- `/proc` enrichment for NPU processes: full command line, user, RSS and
  container attribution from the cgroup path, with `docker ps` names when
  docker is reachable (2 s deadline, 30 s cache). A `CTN` column shows the
  container, `host`, or `?` for a PID that has no `/proc` entry in the
  current PID namespace. `npu-smi` 26.x adds a `Process id in container`
  column; when a host PID is invisible but the container PID resolves —
  i.e. the process shares our container — enrichment still works and the
  column reads `local`.
- The middle NPU pane follows focus: card metrics while navigating cards,
  selected-process details (command line, user, container, RSS) while
  navigating processes.
- `--once` text output gains a `CTN` column; `--json` process objects gain
  `container_pid`, `visible`, `user`, `container` and `cmdline` fields.
- btop-style dressing on the NPU view: fill meters on the AICORE and HBM
  columns, `▁▂▃▄▅▆▇█` sparklines of AICore and HBM history on the selected
  card (one point per round, ~4 min kept), and rounded borders.
- `npu/procfs`: the only module that reads `/proc`; injectable roots keep
  the parsers unit-testable.
- Fixture-based parser tests (`tests/fixtures/`, `tests/npu_parse.rs`), so
  the text layer is covered on CI machines without an NPU. Captured
  samples cover `npu-smi` 24.1, 25.2 and 26.1 (`910B1`), including the
  26.x in-container PID column.
- `npu` module: pure card/process model, `npu-smi` parser with a deadline
  on every subprocess, and a latest-wins collector thread.
- Crate is now a library plus a thin binary, so integration tests can link
  against the public API (`tests/api.rs`).

### Changed

- One NPU sampling round is now orchestrated by `NpuCollector`: `info`
  plus `usages`/`common` for the selected card, one `ecc` card and
  process enrichment — four `npu-smi` calls per round plus a `docker ps`
  every 30 s, all on the collector thread.
- `UsageSample` / `CommonSample` / `EccSample` moved into `npu::model`
  (pure data belongs to the pure layer); `npu::smi` re-exports them so
  existing paths keep working.
- The default view is the NPU screen when `npu-smi` is present; without it
  utop falls back to the original system view.
- Killing a host-side PID from inside a container reports "not reachable
  from this namespace" instead of the misleading "no longer exists". Kill
  now targets the *container* pid for same-container processes (`npu-smi`
  26.x), so a visible worker is actually signalable from inside its own
  container instead of always failing with ESRCH.
- The module boundary is now real instead of aspirational: `collect` is
  literally the only module that imports sysinfo. The UI reads snapshot
  structs (`SysStats` / `ProcDetails`) stored on `App`, the event loop
  reaches `System` only through `collect`, and PID / signal types are local
  to `model`.
- Process refresh no longer collects per-process disk usage, which was
  never displayed. Threads remain listed (they are rows too).
- Killing reports a structured outcome (`KillOutcome`) instead of only a
  status string; messages now say SIGTERM / SIGKILL.
- Sort / reverse in tree view re-shape the already-collected rows instead
  of re-reading the process table.
- Input handlers are pure (they record the work they need via `Followup`
  and let the event loop perform it), which made them unit-testable.
- The ~220 ms startup CPU-sampling gap now runs before the terminal is
  taken over, so there is no blank unresponsive screen at startup.
- Terminal teardown is shared between the clean-exit and panic paths.

### Fixed

- The `npu-smi -v` startup probe now has a 2 s deadline and is killed when it
  hangs, matching the "every subprocess has a deadline" rule. Previously a
  wedged driver blocked `run()` before raw mode was even enabled, leaving the
  TUI invisible with an unreaped child.
- Terminal initialisation failures after `enable_raw_mode()` now restore the
  terminal before returning: a failed `EnterAlternateScreen`/mouse-capture or
  `Terminal::new` no longer leaves the shell in raw mode behind the alternate
  screen.
- `npu-smi` 25.x writes HBM usage as `57636/ 65536` without a space before
  the slash; this used to parse as zero. Both the spaced (24.x) and tight
  (25.x) forms are now accepted.
- ECC isolated-page counters use `HBM Single/Double Bit Isolated Pages
  Count` on 25.x; the parser now matches those keys as well.
- `npu-smi` is spawned with the common Ascend driver library directories
  when `LD_LIBRARY_PATH` is unset, covering containers where the tool is
  not registered with `ldconfig` (observed on a real 910B2 host).
- The `Cmd` line in the details panel was always empty: process command
  lines were never sampled.
- Details panel memory was labelled "MB" for MiB values; now "MiB".
- Sort indicator was missing from the NAME column header.
- The 0.4.0 entry below said startup samples are "spaced 150 ms apart";
  the actual gap is `MINIMUM_CPU_UPDATE_INTERVAL + 20 ms` (~220 ms on
  Linux). Recorded here rather than rewriting history.
- Terminal restore on panic now also shows the cursor again (previously
  only the clean-exit path did).

## [0.4.0] - 2026-08-05

### Added

- Load average (1/5/15 min, colored relative to core count) and uptime
  in the summary panel.
- Process count in the table title: `Processes (N)`.
- `-V` / `--version` flag.
- Status-line feedback when adjusting the refresh interval with `-` / `+`.
- CI now also runs on macOS; package metadata (repository, readme,
  keywords, categories) in Cargo.toml.

### Changed

- Startup only samples CPU, memory and processes — disks, networks,
  components and users were enumerated twice but never displayed
  (~560 ms of wasted work removed, startup is ~2x faster).
- The terminal is restored if utop panics, instead of leaving the
  shell stuck in raw mode behind the alternate screen.
- Migrated to Rust edition 2024; two nested `if` blocks collapse into
  `let`-chains (now stable).
- Summary is split into three lines (stats / mode / key help); the old
  216-character single line was truncated on terminals under ~200
  columns.
- The NAME column now fills the remaining table width instead of a
  fixed 55%.

### Fixed

- First frame showed 0.0% CPU everywhere: the two startup samples were
  taken back-to-back with no interval between them. Samples are now
  spaced 150 ms apart, so the first paint already has real numbers.
- Process memory column and details panel displayed KiB labeled as MB;
  the summary displayed MiB labeled as GiB. Both are now true MiB/GiB.
- Collapsed-subtree markers of dead processes were kept forever; the
  set is now pruned on every rebuild.

## [0.3.0] - 2026-07-28

### Added

- Per-core CPU meters in the summary panel with load coloring
  (green < 60%, yellow < 85%, red ≥ 85%).
- Load-colored CPU% values in the process table and summary.
- Tree view (`t`) built from parent PIDs, with collapsible subtrees
  (Space to collapse/expand); sorting applies within sibling groups.
- Kill confirmation with signal choice: `k` then `y` = SIGTERM,
  `K` = SIGKILL, Esc = cancel.
- Command-line options: `--sort`, `--asc`, `--delay`, `--filter`, `--tree`,
  `--help`.
- Mouse wheel scrolling of the process list.
- PPID shown in the process details panel.
- CI workflow (rustfmt, clippy with `-D warnings`, tests, release build).
- This changelog.

### Changed

- Upgraded `sysinfo` from 0.29 to 0.38 (migrated off the removed
  `SystemExt`/`ProcessExt`/`CpuExt`/`PidExt` trait APIs).
- Split the single 664-line `main.rs` into focused modules:
  `app`, `proc`, `collect`, `ui`, `cli`.
- Status messages now auto-clear after a few ticks instead of lingering
  forever.
- Ctrl+C now exits from any input mode (raw mode swallows SIGINT, so it is
  handled as a key event).

### Fixed

- Search mode no longer rebuilds the process list twice when Esc is pressed.
- Sorting and filtering no longer re-allocate a lowercased copy of every
  process name on each comparison; the lowercase name is precomputed once
  per snapshot.

## [0.2.0] - earlier

- Single-crate rewrite: ratatui-based TUI with sorting, filtering, pause,
  kill, and a details panel.
