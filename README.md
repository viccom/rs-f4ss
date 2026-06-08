<div align="center">

# rs-f4ss

### Mount remote filesystems. Share local directories. Peer-to-peer over WebDAV/HTTP.

**rs-f4ss** is a multi-protocol remote filesystem mount client and a self-hosted file
sharing server, written in Rust. It mounts WebDAV and HTTP static-file servers as native
filesystems (FUSE on Linux, WinFsp on Windows), and can itself serve directories for
peer-to-peer file sharing between rs-f4ss instances.

[![CI](https://github.com/viccom/rs-f4ss/actions/workflows/ci.yml/badge.svg)](https://github.com/viccom/rs-f4ss/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.2.0-blue.svg)](Cargo.toml)
[![Tests](https://img.shields.io/badge/tests-205%2B%20passing-brightgreen.svg)](#testing)
[![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20Windows-lightgrey.svg)](#supported-platforms)

[Quick Start](#-quick-start) •
[Features](#-features) •
[Architecture](#-architecture) •
[Documentation](#-documentation) •
[Contributing](CONTRIBUTING.md) •
[Changelog](CHANGELOG.md)

</div>

---

> **AI 协助开发 · AI-Assisted Development**
>
> 本项目的需求分析、架构设计、代码实现、测试用例与文档编写,**完全由 AI 编程助手生成**;
> 项目维护者承担 prompt 工程、需求定义、人工审阅验证、缺陷修正与发布工程。
>
> 主要使用的大语言模型(按开发时间顺序):
>
> | 模型 | 阶段 / 贡献 |
> |------|------------|
> | 智谱AI GLM-5.1 | 早期架构 · 核心 FUSE/WinFsp 适配层 |
> | DeepSeek-V4-Pro | 服务端 · 协议栈(WebDAV / HTTP)· CI 反复修复 |
> | MiniMax-M3 (经 Claude Code 2.1.150) | Vue 3 UI · 跨平台发布 · 文档与 TDD 编写 |
>
> 使用方式、提交记录与质量保证过程均可在 [commit history](../../commits/master)、
> [CI 报告](../../actions) 与 [CHANGELOG.md](CHANGELOG.md) 中追溯。
> 如发现 AI 生成内容存在偏差,欢迎通过 [Issue](../../issues) 反馈。

---

## ✨ Features

### 📂 Mount — Turn remote servers into local filesystems
- **WebDAV** backend (dufs, Nginx, Apache, Nextcloud, ...)
- **HTTP static** backend (nginx autoindex, Caddy, Python http.server, ...)
- **FUSE** on Linux, **WinFsp** on Windows — works with every standard tool (`cat`, `cp`, `mv`, `rm`, `find`, `git`, ...)
- Read-write **and** read-only modes
- HTTP Range-based chunked reads with full-download fallback
- Adaptive prefetch (sequential-pattern detection + bandwidth estimation)
- Tokio-backed async runtime, moka-based LRU cache (configurable TTL)
- HTTP Basic Auth, custom headers, redirect-aware

### 📤 Share — Serve local directories over HTTP + WebDAV
- Standalone file server (no API/UI needed): `rs-f4ss share serve`
- HTTP with nginx-format autoindex (parses identically to the HTTP client)
- WebDAV server: PROPFIND, MKCOL, MOVE, COPY, LOCK, DELETE
- Single-range Range requests, 2 GB upload limit, Basic Auth
- "Celadon Glass" browser viewer (Prism syntax highlighting, marked markdown, DOMPurify XSS guard, focus-trap modal, responsive grid)

### 🎛️ Manage — REST API + Web UI
- 9 REST endpoints under `/api/*` — single instance only
- Embedded Vue 3 SPA (no build step, served from the binary)
- Mount manager (list/add/del/start/stop + JSON persistence)
- Share manager (same lifecycle, separate listen address)
- `rs-f4ss mount ...` / `rs-f4ss share ...` CLI subcommands talk to the API

### 🖥️ Desktop — Tauri app with system tray
- Single-instance lock, tray menu, window management
- Cross-compile from Linux with `cargo xwin` (MSVC target)
- WinFsp bundled as runtime dependency

---

## 🚀 Quick Start

### Build from source

```bash
git clone https://github.com/viccom/rs-f4ss.git
cd rs-f4ss
cargo build --release
sudo cp target/release/rs-f4ss /usr/local/bin/
```

### Mount a remote WebDAV server

```bash
# Start dufs as a test server (in another terminal)
dufs /tmp/share --port 5000 --allow-all

# Mount it — that's it
rs-f4ss http://localhost:5000 /mnt/remote

# Use like a local directory
ls /mnt/remote/
cat /mnt/remote/hello.txt
cp /etc/hosts /mnt/remote/hosts.bak
fusermount -u /mnt/remote   # unmount when done
```

### Share a local directory

```bash
# Serve a directory over HTTP + WebDAV on port 8080
rs-f4ss share serve ~/Documents --listen :8080 --allow-all

# Open http://localhost:8080 in a browser
# Or mount it from another machine:
rs-f4ss http://server:8080 /mnt/shared
```

> **Tip:** The server's output format is exactly what the client expects — round-trip P2P sharing works out of the box.

---

## 📦 Installation

### Pre-built binaries (recommended)

Download from [Releases](https://github.com/viccom/rs-f4ss/releases):

| Platform | File |
|----------|------|
| Linux x86_64 | `rs-f4ss-linux-x86_64.tar.gz` |
| Windows x86_64 (MSVC) | `rs-f4ss-windows-x86_64.zip` |

### From source

```bash
cargo install --git https://github.com/viccom/rs-f4ss rs-f4ss-cli
```

### Package managers

_Coming soon — see [Roadmap](#-roadmap)._

### Windows requirements

[WinFsp](https://winfsp.dev) must be installed before mounting.

---

## 🏗️ Architecture

```
                    ┌─ Linux ──────────────────────────────────────┐
                    │  mount_linux.rs: fuser::Filesystem impl       │
                    │  (inode-based: stable inodes via FNV-1a)     │
FuseAdapter ────────┤                                               │
(async methods +    │  Data: inodes (INodeNo → Path)               │
 cache/handles)     ├─ Windows ────────────────────────────────────┤
                    │  mount_windows.rs: WinFspAdapter              │
                    │  (path-based: WinFsp is path-oriented)        │
                    └──────────────────────────────────────────────┘
                              │
                     StorageBackend trait (async)
                              │
                    ┌─────────┼──────────┐
                    │         │          │
              WebDavBackend  HttpBackend S3Backend
               (webdav)      (http)     (planned)
```

### Crate layout

| Crate | Purpose |
|-------|---------|
| `rs-f4ss-core` | Library: `FuseAdapter`, `StorageBackend` trait, all backends, REST API, file server |
| `rs-f4ss-cli` | Binary: `mount` / `serve` / `share` subcommands, daemon mode |
| `rs-f4ss-desktop` | Tauri app: tray icon, GUI for mount manager |

### Key design decisions

- **No `VirtualFs` trait** — Linux (inode) and Windows (path) are fundamentally different. Each platform directly calls `FuseAdapter`'s async methods.
- **Sync→Async bridge** — `FuseAdapter` owns a private `tokio::Runtime`; FUSE/WinFsp callbacks call `self.block_on()`.
- **Write buffering** — Writes accumulate in `HandleTable` (up to 2 GiB). Data uploads on `flush`/`release` (full PUT, no partial writes).
- **Read strategy** — Dirty write buffer → per-handle read cache → prefetch → backend. Backend uses HTTP Range with full-download fallback.
- **cfg-gated deps** — `fuser` is Linux-only, `winfsp` is Windows-only. Core code compiles on all platforms.
- **Feature flags** — `webdav` (default), `http`, `api`, `serve`. Pick exactly what you need.

See [docs/ADR.md](docs/ADR.md) for the full decision log.

---

## 📚 Usage

### Mount a remote server

```bash
# Basic
rs-f4ss http://server:5000 /mnt/remote

# With authentication
rs-f4ss http://server:5000 /mnt/remote --user admin --pass secret

# Read-only, foreground (don't daemonize)
rs-f4ss http://server:5000 /mnt/remote --read-only --foreground

# Custom cache TTL and size
rs-f4ss http://server:5000 /mnt/remote --cache-ttl 120 --cache-size 10000

# Unmount
rs-f4ss unmount /mnt/remote
# or:  fusermount -u /mnt/remote  (Linux)
# or:  rs-f4ss status             # list all mounts
```

### Start the management server

```bash
# Single-instance API + Web UI on :8080
rs-f4ss serve --listen 0.0.0.0:8080

# Open the Web UI:  http://localhost:8080
# API base URL:     http://localhost:8080/api
```

### Manage mounts via CLI (talks to the API)

```bash
rs-f4ss mount list                              # List configured mounts
rs-f4ss mount add myserver --url http://x:5000 --path /mnt/data
rs-f4ss mount start myserver                    # Start the mount
rs-f4ss mount stop myserver                     # Stop the mount
rs-f4ss mount del myserver                      # Remove the config
```

### Share a local directory

```bash
# Standalone (no API server required)
rs-f4ss share serve /path/to/dir --listen :8080
rs-f4ss share serve /path/to/dir --listen :8080 --user admin --pass secret
rs-f4ss share serve /path/to/dir --listen :8080 --read-only

# Managed via the API server
rs-f4ss share list
rs-f4ss share add myfiles --path /data --listen :8081
rs-f4ss share start myfiles
rs-f4ss share stop myfiles
rs-f4ss share del myfiles
```

---

## 🔌 Supported Protocols & Platforms

### Protocols

| Protocol | Status | Tested with |
|----------|:------:|-------------|
| **WebDAV** | ✅ Stable | dufs, Nginx, Apache, Nextcloud |
| **HTTP static** | ✅ Stable | nginx autoindex, Caddy, Python http.server |
| S3 | 🔜 Planned | AWS S3, MinIO |
| SFTP/FTP | 🔜 Planned | OpenSSH, vsftpd |
| 百度网盘 | 🔜 Planned | — |

### Platforms

| Platform | Status | Driver | Install |
|----------|:------:|--------|---------|
| **Linux** | ✅ Stable | [FUSE 3](https://github.com/libfuse/libfuse) | `apt install fuse3 libfuse3-dev` |
| **Windows** | ✅ Stable | [WinFsp](https://winfsp.dev) | Installer from winfsp.dev |
| macOS | 🔜 Planned | macFUSE | — |

---

## 🔧 Build Options

```bash
# Minimal — WebDAV only (the default)
cargo build --release

# WebDAV only (explicit, no defaults)
cargo build --no-default-features --features webdav

# HTTP static file backend
cargo build --no-default-features --features http

# REST API + embedded Web UI
cargo build --features webdav,api

# File sharing server
cargo build --features webdav,http,api,serve

# Everything
cargo build --all-features
```

### Cross-compile to Windows from Linux

```bash
# CLI: cargo-xwin + MSVC target
cargo xwin build --release -p rs-f4ss-cli --all-features \
    --target x86_64-pc-windows-msvc

# Desktop Tauri app
cargo xwin build --release -p rs-f4ss-desktop \
    --target x86_64-pc-windows-msvc
```

---

## 🧪 Testing

We follow strict TDD. Every feature starts with a failing test.

```bash
# Unit tests (no FUSE required, fast)
cargo test --all-features                     # 205+ tests
cargo test --lib -- cache                    # all tests matching "cache"
cargo test test_parse_propfind -- --nocapture # one test, with output

# Lint & format (CI gates — must be zero)
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -W clippy::all

# E2E (requires /dev/fuse + dufs)
bash tests/e2e.sh
bash tests/e2e-api.sh
bash tests/e2e-share.sh

# Cross-platform E2E (Windows, requires WinFsp)
powershell -File tests/e2e.ps1
```

See [docs/TDD.md](docs/TDD.md) for our test methodology, and
[docs/TEST_PLAN.md](docs/TEST_PLAN.md) for the full test matrix.

### Test counts

| Suite | Count |
|-------|------:|
| Unit tests (lib + bins, all features) | 205+ |
| E2E bash (Linux FUSE) | 51 |
| E2E PowerShell (Windows WinFsp) | 51 |
| E2E API | 43 |

---

## ⚙️ Configuration

### CLI flags (mount)

| Flag | Default | Description |
|------|---------|-------------|
| `--user <USER>` | — | HTTP Basic Auth username |
| `--pass <PASS>` | — | HTTP Basic Auth password |
| `--pass-file <FILE>` | — | Read password from file (chmod 600) |
| `--read-only` | `false` | Reject all writes (EROFS) |
| `--foreground` | `false` | Don't daemonize |
| `--cache-ttl <SEC>` | `60` | Metadata cache TTL |
| `--cache-size <N>` | `1000` | Maximum cached entries |

### Environment variables

| Var | Effect |
|-----|--------|
| `DUFS_MOUNT_PASSWORD` | Password (overrides `--pass`, useful in CI) |
| `RUST_LOG` | tracing-subscriber filter, e.g. `rs_f4ss=debug,fuser=info` |
| `XDG_STATE_DIR` | Where the daemon stores PID + log files |

### Mount config persistence

When started via the API, mount configurations are persisted to
`$XDG_STATE_DIR/rs-f4ss/mounts.json` and auto-restored on next launch.

---

## 🗺️ Roadmap

| Phase | Status | Description |
|-------|:------:|-------------|
| **Phase 1** | ✅ | WebDAV + FUSE/WinFsp + CLI |
| **Phase 2** | ✅ | HTTP backend, REST API, Web UI, Desktop app, Daemon mode, File sharing server |
| **Phase 3** | 🔜 | S3 backend, macOS support, 百度网盘 / FTP / SFTP backends, release binaries via CI |

See [docs/TASKS.md](docs/TASKS.md) for the detailed task breakdown and
[docs/SPEC-v2.md](docs/SPEC-v2.md) for the Phase 2 specification.

---

## 📖 Documentation

| Document | Description |
|----------|-------------|
| [CLAUDE.md](CLAUDE.md) | Architecture, build commands, file structure (for AI assistants) |
| [docs/SPEC.md](docs/SPEC.md) | Phase 1 specification |
| [docs/SPEC-v2.md](docs/SPEC-v2.md) | Phase 2 expansion specification |
| [docs/ADR.md](docs/ADR.md) | Architecture decision records |
| [docs/TDD.md](docs/TDD.md) | Test-driven development methodology used in this project |
| [docs/TEST_PLAN.md](docs/TEST_PLAN.md) | Full test plan and acceptance criteria |
| [docs/DEV_GUIDE.md](docs/DEV_GUIDE.md) | Developer setup and workflow guide |
| [docs/TASKS.md](docs/TASKS.md) | Task breakdown and progress tracking |
| [CHANGELOG.md](CHANGELOG.md) | Release notes (Keep a Changelog format) |
| [CONTRIBUTING.md](CONTRIBUTING.md) | How to contribute |
| [LICENSE](LICENSE) | MIT License |

---

## 🤝 Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md) first — it
covers our TDD workflow, commit message conventions (Conventional Commits), and the PR
process.

- 🐛 **Bug reports** → [open an issue](../../issues/new?template=bug_report.md)
- 💡 **Feature requests** → [open an issue](../../issues/new?template=feature_request.md)
- 🔧 **Pull requests** → fork → branch → tests → PR

Before submitting a PR, make sure:

- [ ] `cargo test --all-features` passes
- [ ] `cargo clippy --all-targets --all-features -- -W clippy::all` is clean
- [ ] `cargo fmt --all -- --check` is clean
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)

---

## 📊 Project stats

| Metric | Value |
|--------|------:|
| Source lines (core + cli + desktop) | ~8,800 |
| Crates | 3 |
| Feature flags | 5 |
| REST API endpoints | 9 |
| Binary size (Linux CLI, stripped) | 7.4 MB |
| Binary size (Windows CLI) | 9.1 MB |
| Binary size (Desktop app) | 10.3 MB |
| Min. Rust version | 1.75 |

---

## 📜 License

[MIT](LICENSE) © 2026 [viccom](https://github.com/viccom)

---

## 🙏 Acknowledgments

- [cberner/fuser](https://github.com/cberner/fuser) — the FUSE bindings that make Linux mounting possible
- [winfsp](https://winfsp.dev) — the Windows filesystem proxy
- [dufs](https://github.com/sigoden/dufs) — the test server we use for E2E (and which inspired the project name)
- [axum](https://github.com/tokio-rs/axum), [tokio](https://tokio.rs), [moka](https://github.com/moka-rs/moka), [quick-xml](https://github.com/tafia/quick-xml) — the Rust ecosystem that makes this project pleasant to write
- [Tauri](https://tauri.app) — the desktop framework
- All our [contributors](../../graphs/contributors) and issue reporters

---

<div align="center">

**If rs-f4ss is useful to you, consider giving it a ⭐ — it helps others find the project.**

</div>
