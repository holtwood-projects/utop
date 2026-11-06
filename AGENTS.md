# AGENTS.md — utop

一个用 Rust 编写的轻量级终端进程监视器（htop 风格），基于 ratatui + crossterm + sysinfo，同时是"库 crate + 薄二进制"分层结构的示例项目。

## 常用命令

```sh
cargo fmt                                   # 格式化（CI 以 cargo fmt --check 校验）
cargo clippy --all-targets -- -D warnings   # 静态检查，CI 以 -D warnings 门禁
cargo test                                  # 单元测试（src 各模块内嵌）+ 集成测试（tests/api.rs）
cargo build --release                       # 构建 release 二进制（CI 同样执行此步）
cargo run --release                         # 本地直接运行 utop
```

## 代码结构

- `src/main.rs` — 薄二进制入口，仅调用 `utop::cli::parse()` 与 `utop::run()`
- `src/lib.rs` — 库 crate 装配与模块地图；公开 `cli`、`model` 与 `run`
- `src/run.rs` — 事件循环，唯一持有终端的非纯外壳
- `src/app.rs` — 应用状态与输入模式（纯逻辑）
- `src/model.rs` — 进程行模型、排序、过滤与树构建（纯逻辑）
- `src/collect.rs` — sysinfo 快照，唯一 import sysinfo 的模块
- `src/ui.rs` — ratatui 渲染，纯渲染，只读 `App` 中的快照
- `src/cli.rs` — 命令行参数解析（纯解析器）
- `src/npu/model.rs` — NPU 卡 / 进程模型、聚合与排序（纯逻辑）
- `src/npu/smi.rs` — `npu-smi` 调用与文本解析，唯一子进程入口
- `src/npu/collect.rs` — 后台采集线程，latest-wins 投递
- `src/npu/report.rs` — `--once` / `--json` 一次性输出
- `tests/api.rs` — 通过公开 API（`utop::cli` / `utop::model`）的集成测试
- `tests/npu_parse.rs` — 基于 `tests/fixtures/` 的 NPU 解析测试（无需真卡）
- `.github/workflows/ci.yml` — CI 定义：fmt / clippy / test / release build

## 关键约束

- 分层严格无环：`model` ← `cli` ← `app` ← {`collect`, `ui`} ← `run`，不得反向依赖
- 除 `run` 与 `collect` 外全部是纯逻辑，单元测试无需真实系统；输入处理器通过 `Followup` 记录后续动作，由事件循环代为执行
- sysinfo 只允许出现在 `collect`；UI 只读快照结构（`SysStats` / `ProcDetails`），不直接触碰 `System`
- `npu-smi` 只允许出现在 `npu/smi.rs`；每次调用必须有超时，且不得在渲染线程执行
- NPU 采集线程与 UI 之间只传纯数据快照；UI 不持有子进程或驱动句柄
- Rust edition 2024，依赖仅 crossterm / ratatui / sysinfo 三个 crate（NPU 采集不引入新依赖）
- rustfmt：max_width 100、4 空格缩进、Unix 换行；.editorconfig 对 yml/json/toml 用 2 空格缩进
- CI 在 ubuntu-latest（x86_64）、ubuntu-24.04-arm（aarch64）与 macos-latest 跑 fmt / clippy / test / release build
- 许可证为 MIT OR Apache-2.0 双许可（LICENSE-MIT / LICENSE-APACHE）

## 文档约定

- CHANGELOG.md：面向用户的变更在合入时写入 [Unreleased]（Keep a Changelog zh-CN 格式）
- 文档全中文
