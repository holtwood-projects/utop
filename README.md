**English** | [中文](#chinese)

<a id="top"></a>

# utop

A lightweight terminal process monitor written in Rust, based on ratatui and sysinfo, inspired by htop.

- **Lightweight**: depends only on the crossterm, ratatui and sysinfo crates, with no system-level dependencies
- **Intuitive**: per-core CPU meters colored by load, process details readable at a glance
- **Example project**: a structurally complete Rust TUI tool — model, state, collection and rendering each have their own role — suitable as a starting point for reading and further development

## Screenshots

![utop tree view + process details](demo.png)

## Features

- Per-core CPU meters, colored by load (green / yellow / red)
- Overview panel showing load averages and uptime
- Process table sortable by CPU, memory, PID and name, with ascending / descending toggle
- Incremental search filtering (matches process name or PID)
- Tree view with collapsible subtrees
- Kill process with a second confirmation and signal selection (SIGTERM / SIGKILL)
- Process details panel (state, PPID, executable, command line)
- Pause / resume refresh, adjustable refresh interval
- Mouse wheel scrolling
- Command-line options: initial sort, filter, refresh interval, view mode

## Ascend NPU mode

When `npu-smi` is available, utop opens on the NPU screen:

- Per-card health, temperature, power, AI Core utilisation and HBM usage
- Chip details: PCIe bus id, HBM used/total, hugepages — plus, for the
  selected card, `npu-smi -t` detail: AIVector/AICPU/Ctrlcpu rates, HBM
  bandwidth, AI Core frequency, core count, MCU temperature
- ECC counters swept one card per round; a `NOTE` column flags `ECC!`
  (uncorrectable/isolated pages), `ecc` (correctable), `idle` (HBM nearly
  full with the AI Core idle for ~5 minutes — the dead-worker signature)
  and `masked` (outside `ASCEND_RT_VISIBLE_DEVICES`, which is echoed in
  the header when set)
- NPU process table aggregated by PID: a process spanning several cards
  appears once, with every card id listed. A `CTN` column attributes each
  process to a container (via `/proc/<pid>/cgroup`, with `docker ps`
  names when docker is reachable) or `host`; `?` marks a PID that is not
  visible in the current PID namespace, and `local` marks a process
  resolved through its in-container PID — i.e. one that shares our
  container (`npu-smi` 26.x reports that column; older tools only offer
  the host PID, so a namespaced container degrades to `?`)
- The middle pane follows focus: card metrics on the card table, and the
  selected process's full command line / user / RSS on the process table
  (`npu-smi` truncates names at ~15 chars — `VLLMEngineCor` gets its
  command line back)
- `F2` switches between the NPU and system views; `Tab` moves focus
  between the card table and the process table

One limit is worth stating up front: `npu-smi` and DCMI expose **card-level**
AI Core utilisation and **process-level** HBM usage, but not per-process
compute usage. The `NPU` column in the process table therefore describes
reserved HBM and card placement, not a per-process utilisation percentage.
A second limit: in containers created for *computing-power allocation*
(vNPU), the usage counters report 0 — an artefact of the allocation, not
of the workload.

For scripts and CI:

```sh
utop --once          # one text sample
utop --once --json   # one JSON sample
```

Each sample round costs four `npu-smi` calls (`info`, `usages` and
`common` for the selected card, one `ecc` card) plus `/proc` enrichment
and a cached `docker ps` — all on a background thread with a deadline on
every call, so a slow or hung driver cannot freeze the TUI.

## Build & Run

```sh
cargo build --release
./target/release/utop
```

Or simply:

```sh
cargo run --release
```

## Usage

```
utop [选项]

选项：
  -h, --help           打印帮助并退出
  -s, --sort <KEY>     初始排序键：cpu | mem | pid | name [默认：cpu]
  -a, --asc            以升序启动 [默认：降序]
  -d, --delay <MS>     刷新间隔（毫秒），范围 100..=5000 [默认：500]
  -f, --filter <STR>   初始进程过滤（匹配名称或 PID）
  -t, --tree           以树状视图启动
      --once           输出一次 NPU 采样后退出
  -j, --json           配合 --once，输出 JSON 而非表格
  -V, --version        打印版本并退出
```

## Keybindings

| Key | Action |
|------|------|
| q / Ctrl+C | Quit |
| Up/Down, PgUp/PgDn, Home/End, mouse wheel | Navigate |
| s | Cycle sort key (CPU / memory / PID / name) |
| r | Toggle ascending / descending |
| / | Search processes (Enter to confirm, Esc to clear) |
| Esc | Clear filter |
| t | Toggle tree view |
| Space | Collapse / expand subtree (tree view) |
| p | Pause / resume refresh |
| F5 | Force refresh |
| k | Kill the selected process (y = SIGTERM, K = SIGKILL, Esc = cancel) |
| d / Enter | Toggle process details |
| - / + | Decrease / increase the refresh interval (step 100 ms) |
| Tab | Switch card / process focus (NPU view) |
| F2 | Switch between the NPU and system views |

Note: when filtering, the tree view is temporarily flattened into a list, because a broken tree is harder to read than a plain list.

## Module Structure

| Module | Responsibility |
|------|------|
| `src/main.rs` | Thin binary entry point |
| `src/lib.rs` | Library crate assembly and module map |
| `src/run.rs` | Event loop (the only non-pure shell, owns the terminal) |
| `src/app.rs` | Application state and input modes (pure logic) |
| `src/model.rs` | Process row model, sorting, filtering and tree building (pure logic) |
| `src/collect.rs` | sysinfo snapshots (the only module that touches sysinfo) |
| `src/ui.rs` | ratatui rendering (pure rendering, reads only the snapshot in App) |
| `src/cli.rs` | Command-line argument parsing (pure parser) |
| `src/npu/model.rs` | NPU card / process model, aggregation, card flags (pure) |
| `src/npu/smi.rs` | `npu-smi` invocation and text parsing (only `npu-smi` site) |
| `src/npu/procfs.rs` | `/proc` enrichment and container attribution |
| `src/npu/collect.rs` | Sampling orchestration and latest-wins collector thread |

The layering is strictly acyclic: `model` ← `cli` ← `app` ← {`collect`, `ui`} ← `run`. Everything except `run` and `collect` is pure logic, so unit tests need no real system.

## Development

```sh
cargo fmt          # 格式化
cargo clippy       # 静态检查（CI 以 -D warnings 门禁）
cargo test         # 单元测试 + 集成测试
```

## License

Dual-licensed under [MIT](./LICENSE-MIT) or [Apache-2.0](./LICENSE-APACHE), and the user may choose either one.

---

<a id="chinese"></a>
[English](#top) | **中文**

# utop

一个用 Rust 编写的轻量级终端进程监视器，基于 ratatui 和 sysinfo，灵感来自 htop。

- **轻量**：仅依赖 crossterm、ratatui、sysinfo 三个 crate，无任何系统级依赖
- **直观**：逐核 CPU 仪表按负载着色，进程详情一眼可读
- **示例项目**：一个结构完整的 Rust TUI 工具——模型、状态、采集、渲染各司其职，适合作为阅读和二次开发的起点

## 截图

![utop 树状视图 + 进程详情](demo.png)

## 功能

- 逐核 CPU 仪表，按负载着色（绿 / 黄 / 红）
- 概览面板显示负载均值与运行时长
- 进程表可按 CPU、内存、PID、名称排序，支持升序 / 降序切换
- 增量搜索过滤（匹配进程名或 PID）
- 树状视图，子树可折叠
- 杀进程带二次确认与信号选择（SIGTERM / SIGKILL）
- 进程详情面板（状态、PPID、可执行文件、命令行）
- 暂停 / 恢复刷新，刷新间隔可调
- 鼠标滚轮滚动
- 命令行参数：初始排序、过滤、刷新间隔、视图模式

## 昇腾 NPU 模式

检测到 `npu-smi` 时，utop 默认进入 NPU 视图：

- 逐卡显示健康状态、温度、功耗、AI Core 利用率与 HBM 占用
- 芯片详情：PCIe 总线号、HBM 已用/总量、大页使用；选中卡另有
  `npu-smi -t` 详情：AIVector/AICPU/Ctrlcpu 利用率、HBM 带宽、
  AI Core 频率、核数、MCU 温度
- ECC 计数每轮扫一张卡；`NOTE` 列给出标记：`ECC!`（不可纠正错误/
  隔离页）、`ecc`（可纠正错误）、`idle`（HBM 接近占满且 AI Core 连续
  约 5 分钟空闲——僵尸占卡的典型特征）、`masked`（不在
  `ASCEND_RT_VISIBLE_DEVICES` 内，头部同时回显该变量）
- NPU 进程表按 PID 聚合：跨多卡的进程只出现一次，NPU 列列出全部卡号。
  `CTN` 列给出容器归属（读 `/proc/<pid>/cgroup`，docker 可达时映射
  容器名）或 `host`；`?` 表示 PID 在当前命名空间不可见；`local` 表示
  进程通过其容器内 PID 解析成功——即与 utop 同容器（`npu-smi` 26.x
  新增的容器内 PID 列使这条路径可用；更早版本只有宿主 PID，普通
  容器内只能降级为 `?`）
- 中间面板随焦点切换：卡表焦点显示卡指标，进程表焦点显示选中进程的
  完整命令行 / 用户 / RSS（`npu-smi` 约 15 字符的截断名被补全——
  `VLLMEngineCor` 能看清是谁了）
- `F2` 在 NPU 视图与系统视图之间切换；`Tab` 在卡表与进程表之间切换焦点

有一个边界需要提前说明：`npu-smi` 和 DCMI 提供**卡级** AI Core 利用率与
**进程级** HBM 占用，但不提供进程级算力利用率。因此进程表的 `NPU` 列表示
显存占用与卡归属，而不是单进程利用率。另一个边界：在算力切分（vNPU）
容器内，利用率计数器恒为 0——那是切分方式的产物，不代表负载情况。

脚本与 CI 用法：

```sh
utop --once          # 输出一次文本采样
utop --once --json   # 输出一次 JSON 采样
```

每轮采样固定四个 `npu-smi` 调用（`info`、选中卡的 `usages` 与 `common`、
一张卡的 `ecc`），加上 `/proc` 增强与有缓存的 `docker ps`，全部运行在
后台线程且每次调用都有超时；驱动变慢或卡死不会冻结 TUI。

## 构建与运行

```sh
cargo build --release
./target/release/utop
```

或者直接：

```sh
cargo run --release
```

## 用法

```
utop [选项]

选项：
  -h, --help           打印帮助并退出
  -s, --sort <KEY>     初始排序键：cpu | mem | pid | name [默认：cpu]
  -a, --asc            以升序启动 [默认：降序]
  -d, --delay <MS>     刷新间隔（毫秒），范围 100..=5000 [默认：500]
  -f, --filter <STR>   初始进程过滤（匹配名称或 PID）
  -t, --tree           以树状视图启动
      --once           输出一次 NPU 采样后退出
  -j, --json           配合 --once，输出 JSON 而非表格
  -V, --version        打印版本并退出
```

## 按键

| 按键 | 动作 |
|------|------|
| q / Ctrl+C | 退出 |
| 上/下、PgUp/PgDn、Home/End、鼠标滚轮 | 导航 |
| s | 切换排序键（CPU / 内存 / PID / 名称） |
| r | 切换升序 / 降序 |
| / | 搜索进程（回车确认，Esc 清除） |
| Esc | 清除过滤 |
| t | 切换树状视图 |
| 空格 | 折叠 / 展开子树（树状视图） |
| p | 暂停 / 恢复刷新 |
| F5 | 强制刷新 |
| k | 杀死选中进程（y = SIGTERM，K = SIGKILL，Esc = 取消） |
| d / 回车 | 切换进程详情 |
| - / + | 减小 / 增大刷新间隔（步长 100 毫秒） |
| Tab | 切换卡表 / 进程表焦点（NPU 视图） |
| F2 | 切换 NPU / 系统视图 |

注：过滤时会临时把树状视图拍平为列表，因为残缺的树比普通列表更难读。

## 模块结构

| 模块 | 职责 |
|------|------|
| `src/main.rs` | 薄二进制入口 |
| `src/lib.rs` | 库 crate 装配与模块地图 |
| `src/run.rs` | 事件循环（唯一的非纯外壳，持有终端） |
| `src/app.rs` | 应用状态与输入模式（纯逻辑） |
| `src/model.rs` | 进程行模型、排序、过滤与树构建（纯逻辑） |
| `src/collect.rs` | sysinfo 快照（唯一碰 sysinfo 的模块） |
| `src/ui.rs` | ratatui 渲染（纯渲染，只读 App 中的快照） |
| `src/cli.rs` | 命令行参数解析（纯解析器） |
| `src/npu/model.rs` | NPU 卡 / 进程模型、聚合与卡标记（纯逻辑） |
| `src/npu/smi.rs` | `npu-smi` 调用与文本解析（唯一 npu-smi 入口） |
| `src/npu/procfs.rs` | `/proc` 进程增强与容器归属 |
| `src/npu/collect.rs` | 采样编排与 latest-wins 后台采集线程 |

分层严格无环：`model` ← `cli` ← `app` ← {`collect`, `ui`} ← `run`。除 `run` 与 `collect` 外全部是纯逻辑，单元测试无需真实系统。

## 开发

```sh
cargo fmt          # 格式化
cargo clippy       # 静态检查（CI 以 -D warnings 门禁）
cargo test         # 单元测试 + 集成测试
```

## 许可证

依据 [MIT](./LICENSE-MIT) 或 [Apache-2.0](./LICENSE-APACHE) 双许可发布，使用者可任选其一。
