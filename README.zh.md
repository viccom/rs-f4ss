<div align="center">

# rs-f4ss

### 挂载远程文件系统 · 分享本地目录 · 通过 WebDAV/HTTP 实现点对点共享

**rs-f4ss** 是一个多协议远程文件系统挂载客户端与自托管文件分享服务器,使用 Rust 编写。
它能将 WebDAV 与 HTTP 静态文件服务器挂载为原生文件系统(Linux 上的 FUSE,Windows 上的 WinFsp),
同时自身也能作为目录服务方,实现 rs-f4ss 实例之间的点对点文件分享。

[![CI](https://github.com/viccom/rs-f4ss/actions/workflows/ci.yml/badge.svg)](https://github.com/viccom/rs-f4ss/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![Version](https://img.shields.io/badge/version-0.2.0-blue.svg)](Cargo.toml)
[![Tests](https://img.shields.io/badge/tests-205%2B%20passing-brightgreen.svg)](#测试)
[![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20Windows-lightgrey.svg)](#支持的协议与平台)

[快速开始](#-快速开始) •
[功能特性](#-功能特性) •
[架构设计](#-架构设计) •
[文档](#-文档) •
[贡献指南](CONTRIBUTING.md) •
[更新日志](CHANGELOG.md)

</div>

**[English](README.md)** | **中文**

---

> **AI 协助开发声明**
>
> 本项目的需求分析、架构设计、代码实现、测试用例与文档编写,**完全由 AI 编程助手生成**。
> 项目维护者承担 prompt 工程、需求定义、人工审阅验证、缺陷修正与发布工程。
>
> 涉及的大语言模型(按开发时间顺序):
>
> | 模型 | 阶段 / 贡献 |
> |------|------------|
> | 智谱AI GLM-5.1 | 早期架构 · 核心 FUSE / WinFsp 适配层 |
> | DeepSeek-V4-Pro | 服务端 · 协议栈(WebDAV / HTTP)· CI 反复修复 |
> | MiniMax-M3 (经 Claude Code 2.1.150) | Vue 3 UI · 跨平台发布 · 文档与 TDD |
>
> 使用方式、提交记录与质量保证过程均可在 [commit history](../../commits/master)、
> [CI 报告](../../actions) 与 [CHANGELOG.md](CHANGELOG.md) 中追溯。
> 如发现 AI 生成内容存在偏差,欢迎通过 [Issue](../../issues) 反馈。

---

## ✨ 功能特性

### 📂 挂载 (Mount) — 将远程服务器变成本地文件系统
- **WebDAV** 后端(dufs、Nginx、Apache、Nextcloud ……)
- **HTTP 静态** 后端(nginx autoindex、Caddy、Python `http.server` ……)
- Linux 上 **FUSE**,Windows 上 **WinFsp** — 可与所有标准工具(`cat`、`cp`、`mv`、`rm`、`find`、`git` ……)无缝配合
- 同时支持 **读写** 与 **只读** 模式
- 基于 HTTP Range 的分块读取,失败时回退到完整下载
- 自适应预取(顺序访问模式检测 + 带宽估算)
- Tokio 异步运行时 · moka LRU 缓存(TTL 可配)
- HTTP Basic Auth · 自定义请求头 · 自动跟随重定向

### 📤 分享 (Share) — 通过 HTTP + WebDAV 服务本地目录
- 独立文件服务模式(无需 API / UI):`rs-f4ss share serve`
- HTTP 服务使用 nginx 格式的 autoindex(与服务端解析器完全一致)
- WebDAV 服务:PROPFIND、MKCOL、MOVE、COPY、LOCK、DELETE
- 单区间 Range 请求,2 GB 上传上限,Basic Auth
- "Celadon Glass" 浏览器文件预览(Prism 语法高亮 · marked Markdown · DOMPurify XSS 防护 · focus-trap 模态框 · 响应式网格)

### 🎛️ 管理 (Manage) — REST API + Web UI
- 9 个 `/api/*` 端点 — 单实例约束
- 嵌入式 Vue 3 SPA(无需构建步骤,二进制内嵌)
- 挂载管理器(列出 / 新增 / 删除 / 启动 / 停止 + JSON 持久化)
- 分享管理器(同样生命周期,独立的监听地址)
- `rs-f4ss mount ...` / `rs-f4ss share ...` CLI 子命令通过 API 通信

### 🖥️ 桌面应用 (Desktop) — Tauri + 系统托盘
- 单实例锁 · 托盘菜单 · 窗口管理
- 支持 Linux 上使用 `cargo xwin` 跨编译(MSVC target)
- 运行时依赖 WinFsp

---

## 🚀 快速开始

### 从源码构建

```bash
git clone https://github.com/viccom/rs-f4ss.git
cd rs-f4ss
cargo build --release
sudo cp target/release/rs-f4ss /usr/local/bin/
```

### 挂载远程 WebDAV 服务器

```bash
# 在另一个终端启动 dufs 作为测试服务器
dufs /tmp/share --port 5000 --allow-all

# 挂载它 —— 就是这么简单
rs-f4ss http://localhost:5000 /mnt/remote

# 像本地目录一样使用
ls /mnt/remote/
cat /mnt/remote/hello.txt
cp /etc/hosts /mnt/remote/hosts.bak
fusermount -u /mnt/remote   # 用完后卸载
```

### 分享本地目录

```bash
# 在 8080 端口通过 HTTP + WebDAV 暴露一个目录
rs-f4ss share serve ~/Documents --listen :8080 --allow-all

# 浏览器打开 http://localhost:8080
# 或从另一台机器挂载它:
rs-f4ss http://server:8080 /mnt/shared
```

> **提示:** 服务端的输出格式与客户端期望的格式完全一致 —— 点对点分享开箱即用。

---

## 📦 安装

### 预编译二进制(推荐)

从 [Releases](https://github.com/viccom/rs-f4ss/releases) 下载:

| 平台 | CLI | 桌面应用 (Tauri) |
|------|-----|-----------------|
| Linux x86_64 | `rs-f4ss-v{ver}-x86_64-unknown-linux-gnu.tar.gz` | `rs-f4ss-desktop-v{ver}-x86_64-unknown-linux-gnu.tar.gz` |
| Windows x86_64 (MSVC) | `rs-f4ss-v{ver}-x86_64-pc-windows-msvc.tar.gz` | `rs-f4ss-desktop-v{ver}-x86_64-pc-windows-msvc.tar.gz` |

### 从源码安装

```bash
cargo install --git https://github.com/viccom/rs-f4ss rs-f4ss-cli
```

### 包管理器

_即将支持 —— 参见 [路线图](#-路线图)。_

### Windows 运行环境

挂载前需先安装 [WinFsp](https://winfsp.dev)。

---

## 🏗️ 架构设计

```
                    ┌─ Linux ──────────────────────────────────────┐
                    │  mount_linux.rs: fuser::Filesystem impl       │
                    │  (基于 inode:FNV-1a 生成稳定 inode 号)        │
FuseAdapter ────────┤                                               │
(async methods +    │  数据:inodes (INodeNo → Path)               │
 cache/handles)     ├─ Windows ────────────────────────────────────┤
                    │  mount_windows.rs: WinFspAdapter              │
                    │  (基于路径:WinFsp 本身按路径寻址)              │
                    └──────────────────────────────────────────────┘
                              │
                     StorageBackend trait (async)
                              │
                    ┌─────────┼──────────┐
                    │         │          │
              WebDavBackend  HttpBackend S3Backend
               (webdav)      (http)     (计划中)
```

### Crate 布局

| Crate | 用途 |
|-------|------|
| `rs-f4ss-core` | 库:`FuseAdapter`、`StorageBackend` trait、所有后端、REST API、文件服务 |
| `rs-f4ss-cli` | 二进制:`mount` / `serve` / `share` 子命令、守护进程模式 |
| `rs-f4ss-desktop` | Tauri 应用:托盘图标、挂载管理 GUI |

### 关键设计决策

- **没有 `VirtualFs` trait** —— Linux(inode 模型)与 Windows(路径模型)本质不同,各自平台直接调用 `FuseAdapter` 的异步方法。
- **同步 → 异步桥接** —— `FuseAdapter` 自带私有 `tokio::Runtime`,FUSE / WinFsp 回调通过 `self.block_on()` 调用。
- **写缓冲** —— 写入数据累积在 `HandleTable`(上限 2 GiB),在 `flush` / `release` 时全量 PUT 上传,不支持部分写。
- **读取策略** —— 脏写缓冲 → per-handle 读缓存 → 预取 → 后端。后端优先使用 HTTP Range,失败时回退到整文件下载。
- **cfg-gated 依赖** —— `fuser` 仅 Linux 编译,`winfsp` 仅 Windows 编译。核心代码可跨平台编译。
- **Feature flags** —— `webdav`(默认)、`http`、`api`、`serve`。按需精确选择。

完整决策记录见 [docs/ADR.md](docs/ADR.md)。

---

## 📚 用法

### 挂载远程服务器

```bash
# 基础用法
rs-f4ss http://server:5000 /mnt/remote

# 带认证
rs-f4ss http://server:5000 /mnt/remote --user admin --pass secret

# 只读、前台运行(不守护进程化)
rs-f4ss http://server:5000 /mnt/remote --read-only --foreground

# 自定义缓存 TTL 与容量
rs-f4ss http://server:5000 /mnt/remote --cache-ttl 120 --cache-size 10000

# 卸载
rs-f4ss unmount /mnt/remote
# 或(Linux):fusermount -u /mnt/remote
# 或:        rs-f4ss status         # 列出所有挂载
```

### 启动管理服务

```bash
# 在 :8080 启动单实例 API + Web UI
rs-f4ss serve --listen 0.0.0.0:8080

# 打开 Web UI:http://localhost:8080
# API 基础 URL:http://localhost:8080/api
```

### 通过 CLI 管理挂载(经由 API 通信)

```bash
rs-f4ss mount list                              # 列出已配置的挂载
rs-f4ss mount add myserver --url http://x:5000 --path /mnt/data
rs-f4ss mount start myserver                    # 启动挂载
rs-f4ss mount stop myserver                     # 停止挂载
rs-f4ss mount del myserver                      # 删除配置
```

### 分享本地目录

```bash
# 独立模式(无需 API 服务)
rs-f4ss share serve /path/to/dir --listen :8080
rs-f4ss share serve /path/to/dir --listen :8080 --user admin --pass secret
rs-f4ss share serve /path/to/dir --listen :8080 --read-only

# 通过 API 服务管理
rs-f4ss share list
rs-f4ss share add myfiles --path /data --listen :8081
rs-f4ss share start myfiles
rs-f4ss share stop myfiles
rs-f4ss share del myfiles
```

---

## 🔌 支持的协议与平台

### 协议

| 协议 | 状态 | 测试对象 |
|------|:----:|----------|
| **WebDAV** | ✅ 稳定 | dufs、Nginx、Apache、Nextcloud |
| **HTTP 静态** | ✅ 稳定 | nginx autoindex、Caddy、Python `http.server` |
| S3 | 🔜 计划中 | AWS S3、MinIO |
| SFTP / FTP | 🔜 计划中 | OpenSSH、vsftpd |
| 百度网盘 | 🔜 计划中 | — |

### 平台

| 平台 | 状态 | 驱动 | 安装方式 |
|------|:----:|------|----------|
| **Linux** | ✅ 稳定 | [FUSE 3](https://github.com/libfuse/libfuse) | `apt install fuse3 libfuse3-dev` |
| **Windows** | ✅ 稳定 | [WinFsp](https://winfsp.dev) | 从 winfsp.dev 下载安装包 |
| macOS | 🔜 计划中 | macFUSE | — |

---

## 🔧 构建选项

```bash
# 最小构建 —— 仅 WebDAV(默认)
cargo build --release

# 显式仅 WebDAV(关闭默认特性)
cargo build --no-default-features --features webdav

# HTTP 静态文件后端
cargo build --no-default-features --features http

# REST API + 嵌入式 Web UI
cargo build --features webdav,api

# 文件分享服务
cargo build --features webdav,http,api,serve

# 全部特性
cargo build --all-features
```

### 从 Linux 交叉编译到 Windows

```bash
# CLI:cargo-xwin + MSVC target
cargo xwin build --release -p rs-f4ss-cli --all-features \
    --target x86_64-pc-windows-msvc

# 桌面 Tauri 应用
cargo xwin build --release -p rs-f4ss-desktop \
    --target x86_64-pc-windows-msvc
```

---

## 🧪 测试

我们严格遵循 TDD —— 每个功能都从一个失败的测试开始。

```bash
# 单元测试(无需 FUSE,速度快)
cargo test --all-features                     # 205+ 个测试
cargo test --lib -- cache                    # 运行名称包含 "cache" 的测试
cargo test test_parse_propfind -- --nocapture # 跑单个测试,带输出

# Lint & 格式(CI 强制项 —— 必须为零警告)
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -W clippy::all

# E2E(需要 /dev/fuse + dufs)
bash tests/e2e.sh
bash tests/e2e-api.sh
bash tests/e2e-share.sh

# 跨平台 E2E(Windows,需要 WinFsp)
powershell -File tests/e2e.ps1
```

测试方法论见 [docs/TDD.md](docs/TDD.md),完整测试矩阵见
[docs/TEST_PLAN.md](docs/TEST_PLAN.md)。

### 测试数量

| 套件 | 数量 |
|------|-----:|
| 单元测试(lib + bins,全特性) | 205+ |
| E2E bash(Linux FUSE) | 51 |
| E2E PowerShell(Windows WinFsp) | 51 |
| E2E API | 43 |

---

## ⚙️ 配置

### CLI 参数(挂载)

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--user <USER>` | — | HTTP Basic Auth 用户名 |
| `--pass <PASS>` | — | HTTP Basic Auth 密码 |
| `--pass-file <FILE>` | — | 从文件读取密码(请 `chmod 600`) |
| `--read-only` | `false` | 拒绝所有写入(返回 EROFS) |
| `--foreground` | `false` | 不进入守护进程模式 |
| `--cache-ttl <SEC>` | `60` | 元数据缓存 TTL(秒) |
| `--cache-size <N>` | `1000` | 最大缓存条目数 |

### 环境变量

| 变量 | 作用 |
|------|------|
| `DUFS_MOUNT_PASSWORD` | 密码(覆盖 `--pass`,便于 CI 使用) |
| `RUST_LOG` | tracing-subscriber 过滤,例如 `rs_f4ss=debug,fuser=info` |
| `XDG_STATE_DIR` | 守护进程存储 PID 与日志文件的目录 |

### 挂载配置持久化

通过 API 启动时,挂载配置会持久化到
`$XDG_STATE_DIR/rs-f4ss/mounts.json`,下次启动时自动恢复。

---

## 🗺️ 路线图

| 阶段 | 状态 | 说明 |
|------|:----:|------|
| **Phase 1** | ✅ | WebDAV + FUSE/WinFsp + CLI |
| **Phase 2** | ✅ | HTTP 后端、REST API、Web UI、桌面应用、守护进程模式、文件分享服务 |
| **Phase 3** | 🔜 | S3 后端、macOS 支持、百度网盘 / FTP / SFTP 后端、CI 自动发布二进制 |

详细任务分解见 [docs/TASKS.md](docs/TASKS.md),
Phase 2 规范见 [docs/SPEC-v2.md](docs/SPEC-v2.md)。

---

## 📖 文档

| 文档 | 说明 |
|------|------|
| [CLAUDE.md](CLAUDE.md) | 架构、构建命令、文件结构(供 AI 助手阅读) |
| [docs/SPEC.md](docs/SPEC.md) | Phase 1 规范 |
| [docs/SPEC-v2.md](docs/SPEC-v2.md) | Phase 2 扩展规范 |
| [docs/ADR.md](docs/ADR.md) | 架构决策记录 |
| [docs/TDD.md](docs/TDD.md) | 本项目使用的 TDD 方法论 |
| [docs/TEST_PLAN.md](docs/TEST_PLAN.md) | 完整测试计划与验收标准 |
| [docs/DEV_GUIDE.md](docs/DEV_GUIDE.md) | 开发者环境与工作流 |
| [docs/TASKS.md](docs/TASKS.md) | 任务分解与进度跟踪 |
| [CHANGELOG.md](CHANGELOG.md) | 发布说明(Keep a Changelog 格式) |
| [CONTRIBUTING.md](CONTRIBUTING.md) | 如何贡献 |
| [LICENSE](LICENSE) | MIT 许可证 |

---

## 🤝 贡献

欢迎贡献!请先阅读 [CONTRIBUTING.md](CONTRIBUTING.md) —— 其中涵盖
TDD 工作流、Conventional Commits 提交规范,以及 PR 流程。

- 🐛 **Bug 报告** → [提交 Issue](../../issues/new?template=bug_report.md)
- 💡 **功能请求** → [提交 Issue](../../issues/new?template=feature_request.md)
- 🔧 **Pull Request** → fork → 新建分支 → 测试 → 提交 PR

提交 PR 前请确认:

- [ ] `cargo test --all-features` 通过
- [ ] `cargo clippy --all-targets --all-features -- -W clippy::all` 无警告
- [ ] `cargo fmt --all -- --check` 无格式问题
- [ ] 提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/)

---

## 📊 项目统计

| 指标 | 数值 |
|------|-----:|
| 源代码行数(core + cli + desktop) | ~8,800 |
| Crate 数量 | 3 |
| Feature flag 数量 | 5 |
| REST API 端点数 | 9 |
| 二进制体积(Linux CLI,stripped) | 7.4 MB |
| 二进制体积(Windows CLI) | 9.1 MB |
| 二进制体积(桌面应用) | 10.3 MB |
| 最低 Rust 版本 | 1.75 |

---

## 📜 许可证

[MIT](LICENSE) © 2026 [viccom](https://github.com/viccom)

---

## 🙏 致谢

- [cberner/fuser](https://github.com/cberner/fuser) —— 让 Linux 挂载成为可能的 FUSE 绑定
- [winfsp](https://winfsp.dev) —— Windows 文件系统代理
- [dufs](https://github.com/sigoden/dufs) —— 我们用于 E2E 的测试服务器(项目名也源自此)
- [axum](https://github.com/tokio-rs/axum)、[tokio](https://tokio.rs)、[moka](https://github.com/moka-rs/moka)、[quick-xml](https://github.com/tafia/quick-xml) —— 让本项目写起来愉悦的 Rust 生态
- [Tauri](https://tauri.app) —— 桌面应用框架
- 所有 [contributors](../../graphs/contributors) 和 issue 报告者

---

<div align="center">

**如果 rs-f4ss 对你有用,欢迎 ⭐ star —— 这能帮助更多人发现本项目。**

</div>
