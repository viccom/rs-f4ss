# WinFsp/WebDAV 读路径改造（Range 钳制 + 窗口模型 + 句柄宽限表）实施计划

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** 消除实测的"读到 EOF → 416 → 整文件下载"灾难路径，并把读缓存从"严格顺序检测 + 单槽 replace"换成 4 MiB 锚定窗口模型 + 句柄宽限表，使读性能不再依赖 WinFsp 内核缓存。

**Architecture:** 分三层改造：(1) 后端层（webdav.rs/http.rs）——416 语义修复（解析 `Content-Range: bytes */N`，EOF 返回空、越界端点钳制重试一次，彻底移除无上限整文件回退）、206 Content-Range 起偏移校验、主读 GET 接入 send_with_retry；(2) 挂载层（新 window.rs + mount.rs + handle.rs）——窗口三件套 covers/fill_window/read_at 替换 prefetch 机制；(3) 决策层——file_info_timeout 保持 5000，读性能完全由用户态负责。设计蓝本为 rs-cloudfs（CyDrive）已验证的 K34/K41 结构。

**Tech Stack:** Rust（tokio、reqwest）、winfsp-rs、fuser；测试用既有 MockBackend（mount.rs:617）与 WebDAV fake-server 手法（webdav.rs read_path_tests）。

---

## 背景与实测证据（为什么改）

2026-09-17/18 的实测（日志代理抓包，见会话记录）：

1. **读到文件末尾触发整文件下载**：读 1 GiB 文件最后 4 MiB，先发 `Range: bytes=<size>-<size+16M>` → dufs 416 → 回退 `read_full_and_slice`（无上限）→ **两次完整 1 GiB GET**，耗时 5.40 s（有效吞吐 0.7 MB/s）。根因三处叠加：
   - `mount.rs:204` 后台预取传 `remaining = u64::MAX`，预取永不感知 EOF；
   - `mount.rs:358-378` 首读 `size<=256KiB` 直接取 `size`（WinFsp 页对齐窗口对 33 字节文件发出 `bytes=0-4095`）；
   - `webdav.rs:457-460` 416 → `read_full_and_slice`（common.rs:154-199，**无大小上限**）。
2. **小文件每次读都是"416 + 200"两次请求**：33 字节 small.txt 读 100 次产生 200×416 + 201×200。
3. **WebDAV 主读 GET 不走重试**（webdav.rs:414-425 直连 client.request，对比元数据路径有 3 次退避重试），瞬时 5xx 直接读失败。
4. **206 响应不校验 Content-Range**（webdav.rs:445-452 直接信任 body）。
5. **内核数据缓存已被 `file_info_timeout(5000)` 关闭**（WinFsp 语义：仅 `u32::MAX`(-1) 启用 CM 数据缓存；5000 只缓存元数据）。恢复 `u32::MAX` 则写路径丢数据（e2e 44/51）。用户态读缓存现状为"单槽 replace-on-fetch + 严格顺序预取"，对随机读/播放器抖动不友好。

对照 rs-cloudfs 的同类实现（已逐处核对源码）：窗口锚定在请求 offset（无需顺序检测）、EOF 钳制贯穿全层（`reader.rs:124/157`）、非 206 即错误（无整文件回退）、close 后 5s 句柄宽限表（`fs.rs:666`）、flush 零副作用。本计划把该结构移植到 rs-f4ss。

## 设计决策（本计划的契约）

| # | 决策 | 理由 |
|---|------|------|
| D1 | 416 一律不做整文件回退 | 实测 1 GiB×2 回退是最大痛点；EOF 语义用 `Content-Range: bytes */N` 判定 |
| D2 | dufs 式"end 越界但 start 合法"的 416 → 按服务端报告的 N 钳制后**重试一次** | 部分服务器对 end>size 回 416 而非裁剪；一次重试覆盖该类服务器 |
| D3 | 窗口模型：每句柄一个窗口，锚定请求 offset，固定 4 MiB，无投机预取 | rs-cloudfs K34 已验证；随机 seek 友好；内存以窗口为界 |
| D4 | 句柄宽限表 5s/64 条，带 size 见证 | rs-cloudfs K41 / rclone `--vfs-handle-caching 5s`；size 见证防"停车后服务端换内容" |
| D5 | `file_info_timeout` 保持 5000（元数据缓存），内核数据缓存正式弃用 | 读性能改由用户态负责；写正确性已依赖 5000（e2e 51/51 vs 44/51 实测） |
| D6 | 每次 read 查一次 attr（moka 命中零网络）获取/刷新 size | 与现状调用频率一致，服务端变化可见性不变 |
| D7 | 删除 BandwidthEstimator/ReadPattern/PrefetchSlot 全套 | 窗口模型替代；clippy `-D warnings` 强制删净；ADR-011 标 Superseded |

**已知取舍（记录，不隐瞒）**：固定 4 MiB 串行窗口在 高延迟×大带宽 链路上顺序吞吐可能低于现状 16 MiB 预取（loopback 实测 181-209 MB/s）。缓解：窗口大小是常量，基准验证若顺序吞吐回退 >20% 则调至 16 MiB（Task 7 步骤）。

**验证总命令**（每 Task 结束跑对应子集，Task 7 跑全量）：

```bash
# Windows（MSVC）
LIBCLANG_PATH=D:/Python312/Lib/site-packages/clang/native \
  cargo +stable-x86_64-pc-windows-msvc test --workspace --all-features
LIBCLANG_PATH=D:/Python312/Lib/site-packages/clang/native \
  cargo +stable-x86_64-pc-windows-msvc clippy --workspace --all-targets --all-features -- -D warnings
# Windows e2e（需 WinFsp + D:\Tools\dufs.exe）
pwsh -NoProfile -ExecutionPolicy Bypass -File tests/e2e.ps1 -DufsExe D:\Tools\dufs.exe
# Linux（WSL2 原生路径克隆后）
cargo test --workspace --all-features && bash tests/e2e.sh && bash tests/e2e-api.sh && bash tests/e2e-share.sh
```

---

### Task 0: 建立隔离工作区与基线

**Files:** 无代码改动。

**Step 1: 建 worktree**（长 feature，≥3 commit 跨 ≥5 文件）
```bash
cd /e/GitHub/rs-f4ss
git worktree add ../rs-f4ss-readpath -b feat/read-window-model
cd ../rs-f4ss-readpath
```

**Step 2: 基线测试绿**
Run: `LIBCLANG_PATH=... cargo +stable-x86_64-pc-windows-msvc test --workspace --all-features`
Expected: 28+268+24 全绿（与 2026-09-17 基线一致）。

**Step 3: 基线性能数字记录**（用于 Task 7 对比）
用 `C:\tmp\rsf4ss-perf\bench.ps1`（如已清理则按其逻辑重建：dufs + 256 MiB 顺序读两遍 + 33 字节文件读 100 遍 + EOF-4MiB 尾读计时），把结果记入本文件末尾"验证记录"节。
Expected: 尾读约 5+ s（416 回退存在），此为待消灭的基线。

---

### Task 1: WebDAV 后端 — 416 语义修复（消灭整文件回退）

**Files:**
- Modify: `crates/rs-f4ss-core/src/backend/webdav.rs:444-520`（read 的 206/416 分支）
- Test: 同文件 `mod read_path_tests`（复用既有 `read_http_request`/`backend_for`/`expect_range` helper）

**Step 1: 写失败测试（416 + `bytes */N` + offset≥N → 空且零额外请求）**

在 `read_path_tests` 追加：

```rust
#[tokio::test]
async fn read_416_at_eof_returns_empty_without_fallback_download() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut requests = 0usize;
        while let Ok((mut stream, _)) = listener.accept() {
            requests += 1;
            let req = read_http_request(&mut stream).unwrap();
            if requests == 1 {
                expect_range(&req, 100, 5); // 文件只有 50 字节
                stream.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\n\
                    Content-Range: bytes */50\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
                tx.send(()).unwrap();
            } else {
                panic!("416 must not trigger a second request, got: {req}");
            }
        }
    });
    let backend = backend_for(addr);
    let data = backend.read("/f.bin", 100, 5).await.unwrap();
    assert!(data.is_empty());
    rx.recv_timeout(Duration::from_secs(2)).unwrap();
}
```

**Step 2: 跑测试确认失败**
Run: `cargo test -p rs-f4ss-core --features webdav read_416_at_eof`
Expected: FAIL（当前实现会发起第二个无 Range 请求 → 测试线程 panic 或连接关闭错误）。

**Step 3: 写失败测试（dufs 式 end 越界 → 钳制重试一次）**

```rust
#[tokio::test]
async fn read_416_end_overrun_retries_clamped_to_server_size() {
    // 文件 50 字节，请求 bytes=40-44 合法但先演示 40-99：
    // 第一响应 416 + bytes */50 → 客户端应以 bytes=40-49 重试
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut s1, _) = listener.accept().unwrap();
        let r1 = read_http_request(&mut s1).unwrap();
        assert!(r1.contains("bytes=40-99"), "first range: {r1}");
        s1.write_all(b"HTTP/1.1 416\r\nContent-Range: bytes */50\r\n\
            Content-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        let (mut s2, _) = listener.accept().unwrap();
        let r2 = read_http_request(&mut s2).unwrap();
        assert!(r2.contains("bytes=40-49"), "clamped retry: {r2}");
        s2.write_all(b"HTTP/1.1 206 Partial Content\r\n\
            Content-Range: bytes 40-49/50\r\nContent-Length: 10\r\nConnection: close\r\n\r\nABCDEFGHIJ").unwrap();
    });
    let backend = backend_for(addr);
    let data = backend.read("/f.bin", 40, 60).await.unwrap();
    assert_eq!(data, b"ABCDEFGHIJ");
}

#[tokio::test]
async fn read_416_without_content_range_returns_empty() {
    // 无 Content-Range 头 → 无法判定，按 EOF 返回空，且不回退整文件
    // （结构同上，唯一响应：416 无头；断言 data.is_empty() 且只收到 1 个请求）
}
```

**Step 4: 实现**——重写 `webdav.rs` read 的 416 分支（替换 457-460 行）：

```rust
if status == 416 {
    // 416 = "range not satisfiable"。按 RFC，服务端应在
    // Content-Range: bytes */<size> 报告当前大小。绝不回退整文件下载：
    // 实测该路径对 1 GiB 文件产生 2 次全量 GET（5.4s 停顿）。
    let total = resp
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(parse_unsatisfied_size);
    super::common::drain_response(resp).await;
    return match total {
        // 服务端报告的剩余量仍覆盖 offset：end 越界型 416（dufs 行为），
        // 钳制到服务端大小后重试一次（内联第二次请求，防无限递归）。
        Some(n) if offset < n => {
            let clamped = ((n - offset).min(u64::from(size)) as u32).max(1);
            self.ranged_get(&url, offset, clamped).await
        }
        // offset 已在 EOF 及之后，或服务端未报告大小：空读。
        _ => Ok(Vec::new()),
    };
}
```

同时把主请求体抽为私有方法 `ranged_get(&self, url, offset, size)`（即现 407-452 行的请求构造 + 206/404/401/403 处理，206 分支在 Task 2 加校验），`read()` 变为薄壳调用它。新增自由函数：

```rust
/// 解析 416 响应的 `Content-Range: bytes */<size>`。
fn parse_unsatisfied_size(v: &str) -> Option<u64> {
    let rest = v.trim().strip_prefix("bytes */")?;
    rest.trim().parse().ok()
}
```

**Step 5: 跑测试确认通过**（三条新测试 + 既有 `read_416_falls_back_to_full_download` **改为删除**——该测试钉死的正是要消灭的行为；删除并在 commit 信息说明）。
Run: `cargo test -p rs-f4ss-core --features webdav read_`
Expected: 全 PASS。

**Step 6: Commit**
```bash
git add crates/rs-f4ss-core/src/backend/webdav.rs
git commit -m "fix(webdav): 416 answers EOF/clamped-retry instead of uncapped full download"
```

---

### Task 2: WebDAV 后端 — 206 校验 + 主读接入重试

**Files:**
- Modify: `crates/rs-f4ss-core/src/backend/common.rs:99-152`（send_with_retry 加超时参数变体）
- Modify: `crates/rs-f4ss-core/src/backend/webdav.rs`（ranged_get 用重试包装 + 206 校验）
- Test: webdav.rs read_path_tests、common.rs 既有测试

**Step 1: 写失败测试（206 Content-Range 起偏移不匹配 → 错误）**

```rust
#[tokio::test]
async fn read_206_wrong_content_range_start_is_an_error() {
    // 请求 bytes=10-14，服务端却答 Content-Range: bytes 0-4/100（写偏防线）
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let req = read_http_request(&mut s).unwrap();
        expect_range(&req, 10, 5);
        s.write_all(b"HTTP/1.1 206\r\nContent-Range: bytes 0-4/100\r\n\
            Content-Length: 5\r\nConnection: close\r\n\r\nhello").unwrap();
    });
    let backend = backend_for(addr);
    let err = backend.read("/f.bin", 10, 5).await.unwrap_err();
    assert!(format!("{err}").contains("wrong range"), "{err}");
}
```

**Step 2: 确认失败**
Run: `cargo test -p rs-f4ss-core --features webdav read_206_wrong`
Expected: FAIL（当前返回 `hello` 而非错误）。

**Step 3: 写失败测试（主读 503 → 重试后 206 成功）**

```rust
#[tokio::test]
async fn read_ranged_get_retries_after_503() {
    // 第一个响应 503，第二个 206 正确 —— 证明主读路径进入了
    // send_with_retry（旧行为是直连 client.request，503 直接报错）。
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut s1, _) = listener.accept().unwrap();
        let _ = read_http_request(&mut s1).unwrap();
        s1.write_all(b"HTTP/1.1 503\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        let (mut s2, _) = listener.accept().unwrap();
        let req = read_http_request(&mut s2).unwrap();
        expect_range(&req, 0, 4);
        s2.write_all(b"HTTP/1.1 206\r\nContent-Range: bytes 0-4/10\r\n\
            Content-Length: 5\r\nConnection: close\r\n\r\nworld").unwrap();
    });
    let backend = backend_for(addr);
    let data = backend.read("/f.bin", 0, 5).await.unwrap();
    assert_eq!(data, b"world");
}
```

**Step 4: 实现**

common.rs 增加超时可变的变体（`send_with_retry` 语义不变，默认 30 s 委托）：

```rust
pub(crate) async fn send_with_retry_timeout(
    &self,
    method: reqwest::Method,
    url: &str,
    headers: Vec<(&str, String)>,
    timeout: Duration,
) -> Result<reqwest::Response, BackendError> {
    // 与 send_with_retry 相同的循环体，唯一差异：req.timeout(timeout)。
    // 读大块（4-16 MiB 慢链路）需要长于 30s 的预算。
    ... // 循环体从 send_with_retry 复制，timeout 参数化
}

pub(crate) async fn send_with_retry(...) -> ... {
    self.send_with_retry_timeout(method, url, headers, Duration::from_secs(30)).await
}
```
（实现时把现 99-152 行循环体移入新变体，`send_with_retry` 变薄壳——机械重构，由既有 `send_with_retry_recovers_from_503` 与 `read_full_error_drains_body_for_connection_reuse` 守护。）

webdav.rs `ranged_get` 的请求构造（原 414-425 行）替换为：

```rust
let range_header = format!("bytes={offset}-{end_inclusive}");
// 主读走重试包装（与元数据路径同权）：瞬时 5xx/连接失败按 3 次退避重试。
// 读是 GET（幂等），should_retry_request 已放行。超时 300s 沿用旧值。
let resp = self
    .http
    .send_with_retry_timeout(
        Method::GET,
        &url,
        vec![("Range", range_header)],
        std::time::Duration::from_secs(300),
    )
    .await?;
```

`ranged_get` 的 206 分支加校验（在取 body 之前）：

```rust
if status == 206 {
    if let Some(cr) = resp.headers().get("content-range").and_then(|v| v.to_str().ok()) {
        if !cr.trim_start().starts_with(&format!("bytes {offset}-")) {
            super::common::drain_response(resp).await;
            return Err(BackendError::Internal(format!(
                "server returned wrong range: requested offset {offset}, got `{cr}`"
            )));
        }
    }
    // 头缺失时容忍：body 长度仍受 reqwest content-length 约束
    ...原有取 body 逻辑...
}
```

**Step 5: 跑测试**（新增 2 条 + 既有 `read_206_returns_partial_body`、`read_206_body_length_mismatch_is_visible`、503-retry 系列）
Run: `cargo test -p rs-f4ss-core --features webdav && cargo test -p rs-f4ss-core common`
Expected: 全 PASS。

**Step 6: Commit**
```bash
git add crates/rs-f4ss-core/src/backend/common.rs crates/rs-f4ss-core/src/backend/webdav.rs
git commit -m "fix(webdav): validate 206 Content-Range start; route primary ranged GET through retry"
```

---

### Task 3: HTTP 静态后端同款修复

**Files:**
- Modify: `crates/rs-f4ss-core/src/backend/http.rs:452-503`（read 的 416/200 分支）
- Test: http.rs 新建 `mod read_path_tests`（helper 从 webdav.rs 复制同模式）

**Step 1: 写失败测试**（与 Task 1 同款三条：`bytes */N` EOF 空返回、end 越界钳制重试、无头空返回；再加一条「200 回退无 Content-Length 时跳过量超 64 MiB → 错误」——用 chunked 响应模拟，跳过计数超过上限即断连返回 Err）。测试代码结构同 Task 1（`static://` 前缀由 `backend_for_http(addr)` 构造 `HttpBackend`）。

**Step 2: 确认失败**
Run: `cargo test -p rs-f4ss-core --features http read_416`
Expected: FAIL。

**Step 3: 实现**——http.rs read 的 416 分支（477-480 行）替换为与 Task 1 相同的 `parse_unsatisfied_size` 策略（helper 提升到 `backend/common.rs` 供两后端共用，`pub(crate) fn parse_unsatisfied_size`）；200 回退（484-499 行）改为流式计数 guard：

```rust
// 服务器忽略 Range：流式跳过 [0, offset) 再取 size。
// guard 与 Content-Length 是否存在无关：跳过量超上限即中止。
const MAX_FALLBACK_SKIP: u64 = 64 * 1024 * 1024;
let mut skipped = 0u64;
... 在现有 skip 循环内累计 skipped，超限则：
    return Err(BackendError::NotSupported(
        "Range-ignoring server: fallback skip exceeded 64 MiB cap".into()));
```

**Step 4: 跑测试 + 既有 http 测试**
Run: `cargo test -p rs-f4ss-core --features http`
Expected: 全 PASS。

**Step 5: Commit**
```bash
git add crates/rs-f4ss-core/src/backend/http.rs crates/rs-f4ss-core/src/backend/common.rs
git commit -m "fix(http): 416 EOF semantics + bounded fallback skip for Range-ignoring servers"
```

---

### Task 4: 窗口模型三件套（window.rs + handle.rs + mount.rs 重写读路径）

**Files:**
- Create: `crates/rs-f4ss-core/src/window.rs`（纯逻辑，无 FUSE/WinFsp 依赖，全平台可测）
- Modify: `crates/rs-f4ss-core/src/lib.rs`（注册模块）
- Modify: `crates/rs-f4ss-core/src/handle.rs`（OpenFile 字段替换）
- Modify: `crates/rs-f4ss-core/src/mount.rs`（read 重写；删 prefetch 机制；MockBackend 加调用记录）
- Delete: `crates/rs-f4ss-core/src/prefetch.rs`
- Modify: `crates/rs-f4ss-core/src/mount_windows.rs` / `mount_linux.rs`（仅当引用了被删 API）

**Step 0: 引用清点**（动手前先跑，确认删除面）
```bash
rg -n 'read_from_cache|set_read_cache|update_read_pattern|is_first_read|get_cache_info|get_read_state|maybe_spawn_prefetch|try_collect_prefetch|abort_prefetch|BandwidthEstimator|ReadPattern|prefetch' crates/
```
预期引用集中在 mount.rs / handle.rs / prefetch.rs；mount_windows.rs / mount_linux.rs / manager.rs 若有引用逐一列入本任务修改清单。

**Step 1: 写失败测试（window.rs 的 TDD 核心，先建测试文件）**

`crates/rs-f4ss-core/tests/window_test.rs`（集成测试，直接面向公开 API）：

```rust
use rs_f4ss_core::window::{self, WindowState};
use std::collections::Mutex... // 用 std::sync::Mutex 记录 fetch 调用

fn fetch_spy(content: &'static [u8]) -> (impl Fn(u64, u32) -> Future..., std::sync::Mutex<Vec<(u64, u32)>>) {
    // 返回闭包：按 (anchor, len) 切片返回内容，并记录调用
}

#[tokio::test]
async fn eof_read_returns_zero_and_fetches_nothing() {
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    st.size = Some(100);
    let (calls, fetch) = fetch_spy(b"x".repeat(100));
    let out = window::read_at(&mut st, 100, 4096, fetch).await.unwrap();
    assert!(out.is_empty());
    assert!(calls.lock().unwrap().is_empty(), "no request past EOF");
}

#[tokio::test]
async fn window_clamps_fetch_to_eof() {
    let mut st = WindowState::new(window::DEFAULT_READ_WINDOW);
    st.size = Some(1024); // EOF 前 1 KiB 开 4 MiB 窗
    let (calls, fetch) = ...;
    let out = window::read_at(&mut st, 0, 1024, fetch).await.unwrap();
    assert_eq!(out.len(), 1024);
    assert_eq!(calls.lock().unwrap()[0].1, 1024, "fetch len clamped to size");
}

#[tokio::test]
async fn reads_within_window_hit_cache() { /* 一次 fetch 服务 3 次连续小读，calls.len()==1 */ }

#[tokio::test]
async fn large_read_spans_multiple_windows() { /* 9 MiB 读 → 3 次 fetch（4+4+1） */ }

#[tokio::test]
async fn never_serves_past_eof_when_backend_overdelivers() { /* fetch 返回超量 → 服务端钳制 */ }

#[tokio::test]
async fn unknown_size_terminates_on_empty_fetch() { /* size=None，fetch 返回空 → 短读返回已填字节 */ }

#[tokio::test]
async fn window_anchors_at_requested_offset() { /* 直接从 offset=999_999 读 → 首个 fetch anchor==999_999 */ }
```

**Step 2: 确认失败**
Run: `cargo test -p rs-f4ss-core --test window_test`
Expected: 编译失败（模块不存在）。

**Step 3: 实现 window.rs（全文）**

```rust
//! 锚定窗口读模型（移植自 cydrive K34）。
//!
//! 每句柄一个窗口，锚定在请求 offset："开窗即预取，无投机 read-ahead"。
//! size 已知时全链路钳制到 EOF：offset ≥ size 的读直接返回空、零网络，
//! 从结构上消灭"416 → 整文件下载"。

use crate::backend::BackendError;

/// 默认窗口 4 MiB（cydrive DEFAULT_READ_WINDOW 同值）。若高延迟链路顺序
/// 吞吐回退 >20%（Task 7 基准），上调此常量至 16 MiB 再测。
pub const DEFAULT_READ_WINDOW: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ReadWindow {
    pub start: u64,
    pub data: Vec<u8>,
}

#[derive(Debug)]
pub struct WindowState {
    /// 已知文件大小（驱动一切 EOF 钳制）；首次 stat 成功前为 None。
    pub size: Option<u64>,
    pub window: Option<ReadWindow>,
    pub window_size: usize,
}

impl WindowState {
    pub fn new(window_size: usize) -> Self {
        Self { size: None, window: None, window_size: window_size.max(4096) }
    }
    fn covers(&self, p: u64) -> bool {
        match &self.window {
            Some(w) => p >= w.start && p - w.start < w.data.len() as u64,
            None => false,
        }
    }
}

/// 读 [offset, offset+size)。窗口 miss 时在请求 offset 处开窗（钳到 EOF），
/// 窗口命中零网络。返回短读（可能为空）表示 EOF 或后端停滞。
pub async fn read_at<F, Fut>(
    state: &mut WindowState,
    offset: u64,
    size: u32,
    fetch: F,
) -> Result<Vec<u8>, BackendError>
where
    F: Fn(u64, u32) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, BackendError>>,
{
    let want = size as u64;
    let mut out: Vec<u8> = Vec::with_capacity(size as usize);
    let mut pos = offset;
    while (out.len() as u64) < want {
        if let Some(sz) = state.size {
            if pos >= sz {
                break; // EOF：不发任何请求
            }
        }
        if !state.covers(pos) {
            let mut len = state.window_size as u64;
            if let Some(sz) = state.size {
                len = len.min(sz - pos); // 开窗即钳到 EOF
            }
            let data = fetch(pos, len as u32).await?;
            if data.is_empty() {
                // 后端在 EOF 之下给了空窗：短读优于在 dispatcher 线程上自旋
                break;
            }
            state.window = Some(ReadWindow { start: pos, data });
        }
        let w = state.window.as_ref().expect("window filled above");
        let consumed = (pos - w.start) as usize;
        let mut n = (w.data.len() - consumed).min((want - out.len() as u64) as usize);
        if let Some(sz) = state.size {
            n = n.min((sz - pos) as usize); // 后端超发也不越过 EOF
        }
        out.extend_from_slice(&w.data[consumed..consumed + n]);
        pos += n as u64;
    }
    Ok(out)
}
```

`lib.rs` 加 `pub mod window;`。

**Step 4: 跑测试确认通过**
Run: `cargo test -p rs-f4ss-core --test window_test`
Expected: 7 条全 PASS。

**Step 5: handle.rs 字段替换（先补失败测试再改）**

handle.rs 单元测试新增：

```rust
#[test]
fn allocate_has_fresh_window_state() {
    let t = HandleTable::new();
    let fh = t.allocate("/a".into());
    let f = t.read_table().get(&fh).unwrap();
    assert!(f.window.window.is_none());
    assert!(f.window.size.is_none());
}
```

然后 `OpenFile`（handle.rs:17-27）改为：

```rust
pub struct OpenFile {
    pub path: Arc<str>,
    pub dirty: bool,
    pub buffer: Vec<u8>,
    /// 锚定窗口读状态（替代 read_cache + read_pattern）。
    pub window: WindowState,
}
```

删除字段 `read_cache`、`read_pattern` 及方法 `read_from_cache`、`set_read_cache`、`update_read_pattern`、`is_first_read`、`get_cache_info`、`get_read_state`（连同其测试）。`allocate` 初始化 `window: WindowState::new(DEFAULT_READ_WINDOW)`。`write_at`/`replace_contents`/`hydrate_contents` 中清 read_cache 的行改为 `file.window.window = None`（写后窗口失效，保证 read-your-own-writes 走 dirty 臂）。

**Step 6: mount.rs 读路径重写（先补失败测试）**

mount.rs 测试区新增（用 MockBackend + 新增的调用记录）：

MockBackend 增加字段与方法：

```rust
read_calls: std::sync::Mutex<Vec<(String, u64, u32)>>,
pub fn read_calls(&self) -> Vec<(String, u64, u32)> {
    recover_lock(self.read_calls.lock()).clone()
}
// read() 实现开头：recover_lock(self.read_calls.lock()).push((path.into(), offset, size));
```

新测试（放在既有 mount 测试旁）：

```rust
#[tokio::test]
async fn read_past_known_eof_makes_no_backend_request() {
    let backend = MockBackend::new();
    backend.add_file("/", "f.bin", 100, &vec![7u8; 100]);
    let adapter = FuseAdapter::new(backend.clone(), &MountConfig::default());
    let fh = adapter.open("/f.bin", false).await.unwrap();
    // 先读一次让 attr 进缓存
    let _ = adapter.read(fh, 0, 10).await.unwrap();
    let calls_before = backend.read_calls().len();
    let out = adapter.read(fh, 100, 4096).await.unwrap(); // offset == size
    assert!(out.is_empty());
    assert_eq!(backend.read_calls().len(), calls_before, "no request at EOF");
    let out2 = adapter.read(fh, 500, 16).await.unwrap();
    assert!(out2.is_empty());
    assert_eq!(backend.read_calls().len(), calls_before, "no request past EOF");
}

#[tokio::test]
async fn repeated_reads_within_window_do_not_refetch() {
    // 4 KiB 文件读 3 次（0..10, 10..20, 20..30）→ backend read_calls == 1
}

#[tokio::test]
async fn random_seek_reads_anchor_new_window_each_miss() {
    // 三个远距离 offset 各读 4 KiB → 3 次 fetch，且 anchor 分别等于请求 offset
}
```

然后重写 `FuseAdapter::read`（mount.rs:333-412）：

```rust
pub async fn read(&self, fh: u64, offset: u64, size: u32) -> Result<Vec<u8>, MountError> {
    let path = self.handles.get_path(fh).ok_or_else(|| {
        MountError::Backend(BackendError::NotFound("Invalid file handle".into()))
    })?;

    // 0. 未落盘写优先可见（不变）
    if let Some(data) = self.handles.read_from_dirty(fh, offset, size) {
        return Ok(data);
    }

    // 1. 取已知大小（moka 命中零网络；与旧 prefetch 分支同频调用，
    //    服务端变化可见性与现状一致）
    let known = self
        .cache
        .get_attr(&path)
        .await
        .ok()
        .map(|c| c.entry.size)
        .filter(|s| *s > 0);

    // 2. 窗口读。持写锁跨 await：句柄内读串行（cydrive K41 同语义）。
    let start = std::time::Instant::now();
    let backend = self.backend.clone();
    let fetch_path: Arc<str> = path.clone();
    let mut files = recover_lock(self.handles.files_write_guard());
    let file = files.get_mut(&fh).ok_or_else(|| {
        MountError::Backend(BackendError::NotFound("Invalid file handle".into()))
    })?;
    file.window.size = known;
    let data = window::read_at(&mut file.window, offset, size, |anchor, len| {
        backend.read(&fetch_path, anchor, len)
    })
    .await
    .map_err(MountError::Backend)?;
    drop(files);

    let elapsed = start.elapsed();
    self.emit(MountEvent::FileRead {
        path: (&*path).into(),
        bytes: data.len() as u64,
        duration_ms: elapsed.as_millis() as u64,
    });
    Ok(data)
}
```

（`files_write_guard` 为 HandleTable 新增的 `pub(crate)` 写锁访问器，或把 read_at 调用封装为 HandleTable 方法 `read_window(&self, fh, offset, size, fetch)` 内部持锁——**采用后者**，锁不外泄。）

同时删除（D7）：`PrefetchSlot`、`bandwidth`、`prefetch` 字段（mount.rs:80-102），`try_collect_prefetch`/`maybe_spawn_prefetch`/`abort_prefetch`/`abort_all_prefetch`（136-252），`release()` 中 `self.abort_prefetch(fh)` 行（mount.rs:455），`destroy` 路径若引用 prefetch 一并清理；删除 `crates/rs-f4ss-core/src/prefetch.rs` 并从 `lib.rs` 移除模块声明。检查 `mount_windows.rs`/`mount_linux.rs` 中 `flush`/`close` 对 `inner.read` 的调用不变（无需改动），仅清理对被删 API 的引用。

**Step 7: 全量测试**
Run: `cargo test -p rs-f4ss-core --all-features && cargo clippy --workspace --all-targets --all-features -- -D warnings`
Expected: 全绿、零警告（clippy 会抓漏删的死代码）。

**Step 8: Commit**
```bash
git add -A crates/rs-f4ss-core
git commit -m "refactor(mount): replace sequential-prefetch with 4MiB anchored window model

EOF-clamped at every layer: reads at/past known size answer empty with
zero backend requests (kills the measured 416->full-download stall)."
```

---

### Task 5: 句柄宽限表（5s / 64 条 / size 见证）

**Files:**
- Modify: `crates/rs-f4ss-core/src/window.rs`（GraceTable）
- Modify: `crates/rs-f4ss-core/src/mount.rs`（open/release 接线）

**Step 1: 写失败测试**

```rust
// window_test.rs 追加
#[test]
fn grace_take_expired_entry_is_rejected() {
    let mut g = window::GraceTable::new(64, std::time::Duration::from_secs(5));
    let now = std::time::Instant::now();
    g.park("/a", window::ReadWindow { start: 0, data: vec![1, 2, 3] }, 3, now);
    assert!(g.take("/a", 3, now + std::time::Duration::from_secs(2)).is_some());
    g.park("/a", window::ReadWindow { start: 0, data: vec![1] }, 3, now);
    assert!(g.take("/a", 3, now + std::time::Duration::from_secs(6)).is_none(), "expired");
}

#[test]
fn grace_take_rejects_size_mismatch() {
    // 停车时 size=3，取车时当前 size=9（服务端换过内容）→ 不复用
}

#[test]
fn grace_park_evicts_beyond_capacity() {
    // park 65 条 → 第 1 条被挤出
}

// mount.rs 集成测试
#[tokio::test]
async fn close_then_reopen_within_grace_reuses_window() {
    let backend = MockBackend::new();
    backend.add_file("/", "f.bin", 8192, &vec![9u8; 8192]);
    let adapter = FuseAdapter::new(backend.clone(), &MountConfig::default());
    let fh1 = adapter.open("/f.bin", false).await.unwrap();
    let _ = adapter.read(fh1, 0, 4096).await.unwrap();
    adapter.release(fh1).await.unwrap();
    let fh2 = adapter.open("/f.bin", false).await.unwrap();
    let out = adapter.read(fh2, 0, 4096).await.unwrap();
    assert_eq!(out.len(), 4096);
    assert_eq!(backend.read_calls().len(), 1, "second open served from grace window");
}

#[tokio::test]
async fn dirty_close_is_not_parked() {
    // 写脏后 release → 重开读 → 必须重新 fetch（read_calls 增加）
}
```

**Step 2: 确认失败**
Run: `cargo test -p rs-f4ss-core --test window_test grace && cargo test -p rs-f4ss-core grace_`
Expected: 编译失败 / FAIL。

**Step 3: 实现 GraceTable（window.rs 追加）**

```rust
/// 关闭句柄的读窗口停车表（cydrive K41 / rclone --vfs-handle-caching 5s）。
/// 治"播放器/资源管理器关了立刻重开"的抖动。带 size 见证：停车后服务端
/// 换过内容（大小变化）即弃用。
pub struct GraceTable {
    entries: std::collections::HashMap<String, (std::time::Instant, u64, ReadWindow)>,
    ttl: std::time::Duration,
    capacity: usize,
}

pub const DEFAULT_GRACE_TTL: std::time::Duration = std::time::Duration::from_secs(5);
pub const DEFAULT_GRACE_CAPACITY: usize = 64; // 容量×窗口 = 256 MiB 上界

impl GraceTable {
    pub fn new(capacity: usize, ttl: std::time::Duration) -> Self { ... }

    /// 停车。超过容量先扫过期，仍满则挤掉最旧。
    pub fn park(&mut self, path: &str, w: ReadWindow, size: u64, now: std::time::Instant) { ... }

    /// 取车：条目新鲜且 size 见证一致才复用（取出即移除）。
    pub fn take(&mut self, path: &str, current_size: u64, now: std::time::Instant)
        -> Option<ReadWindow> { ... }
}
```

mount.rs 接线：

```rust
// FuseAdapter 字段
grace: std::sync::Mutex<window::GraceTable>,
// new() 初始化 GraceTable::new(DEFAULT_GRACE_CAPACITY, DEFAULT_GRACE_TTL)

// open()：仅当 grace 有该 path 条目时才查 attr 做 size 见证（cold path 零开销）
pub async fn open(&self, path: &str, write: bool) -> Result<u64, MountError> {
    if write && self.read_only { ...不变... }
    let fh = self.handles.allocate(path.to_string());
    if !write {
        if recover_lock(self.grace.lock()).contains_key(path) {
            let cur = self.cache.get_attr(path).await.ok()
                .map(|c| c.entry.size).unwrap_or(0);
            if let Some(w) = recover_lock(self.grace.lock()).take(path, cur, Instant::now()) {
                self.handles.adopt_window(fh, w);
            }
        }
    }
    Ok(fh)
}

// release()：非 dirty 且有窗口 → 停车（在 remove 之后、写回逻辑之外）
let open_file = self.handles.remove(fh);
if let Some(file) = &open_file {
    if !file.dirty {
        if let Some(w) = file.window.window.take()... // 注意 OpenFile 已被 remove 拿走，直接读字段
        { recover_lock(self.grace.lock()).park(&file.path, w, file.window.size.unwrap_or(0), Instant::now()); }
    }
}
// 其余 dirty 写回逻辑不变
```

HandleTable 加 `adopt_window(&self, fh, w)`（写锁注入窗口）。

**Step 4: 跑测试 + clippy + e2e 一轮**
Run: `cargo test -p rs-f4ss-core --all-features && cargo clippy ... -D warnings && pwsh tests/e2e.ps1 ...`
Expected: 全绿；e2e 51/51（重点盯 Test 48-51 服务端变化传播——size 见证保证）。

**Step 5: Commit**
```bash
git add crates/rs-f4ss-core
git commit -m "feat(mount): handle grace table (5s/64) reuses read windows across close/reopen"
```

---

### Task 6: file_info_timeout 决策落地（注释修正 + ADR）

**Files:**
- Modify: `crates/rs-f4ss-core/src/mount_windows.rs:995-1013`（注释块）
- Modify: `docs/ADR.md`（ADR-011 标 Superseded；新增 ADR-013/014）

**Step 1: 修正过时注释**。现 995-1000 行注释声称"OS handles read-ahead and file data caching internally"，与 `file_info_timeout(5000)` 的实际语义（不缓存文件数据）矛盾。替换为：

```rust
// 读性能策略（ADR-014）：内核数据缓存已弃用 —— file_info_timeout 保持
// 有界（5000ms，仅元数据缓存），读吞吐由用户态锚定窗口 + 句柄宽限表
// 负责（window.rs）。历史上 u32::MAX 启用过 CM 数据缓存，但与
// "cleanup 时整文件 PUT"的写模型冲突：CM 延迟写回导致脏页在 cleanup
// 清洗中被丢弃（e2e 44/51，2026-09-17 实测），故弃用。
.file_info_timeout(5000)
```

**Step 2: ADR 更新**。`docs/ADR.md`：
- 索引表 ADR-011 状态 Accepted → **Superseded by ADR-013**；
- 新增 ADR-013「Read Model: 4 MiB Anchored Window (no speculative prefetch)」Accepted，正文引用本计划与实测；
- 新增 ADR-014「WinFsp Kernel Data Cache: Disabled by Design (file_info_timeout=5000)」Accepted，正文记录 5000/u32::MAX 的实测取舍数据（51/51 vs 44/51、210 vs 9708 MB/s）。

**Step 3: Commit**
```bash
git add crates/rs-f4ss-core/src/mount_windows.rs docs/ADR.md
git commit -m "docs(adr)+fix(comment): window read model supersedes prefetch; kernel data cache formally disabled"
```

---

### Task 7: 全量验证、基准对比与收尾

**Step 1: 全量测试（双平台）**
- Windows: `cargo test --workspace --all-features` + `clippy -D warnings` + `cargo fmt --all -- --check`
- WSL（worktree 克隆到原生路径）: `cargo test --workspace --all-features` + `bash tests/e2e.sh && bash tests/e2e-api.sh && bash tests/e2e-share.sh`

**Step 2: e2e.ps1 三轮**（写入复杂性回归的守门）
Run: `pwsh -NoProfile -ExecutionPolicy Bypass -File tests/e2e.ps1 -DufsExe D:\Tools\dufs.exe` ×3
Expected: 3×51/51。

**Step 3: 基准对比（Task 0 基线 vs 现在）**，同机同脚本：
- 尾读（EOF-4MiB 读到尾）：基线 ~5.4 s → **预期 <0.2 s**（EOF 零请求/钳制窗）
- small.txt ×100：基线 155-195 ms → 预期显著下降（416 消失 + 窗口/宽限命中）
- 顺序 256 MiB 第一遍：与基线（181-209 MB/s）对比；**若回退 >20%，把 `DEFAULT_READ_WINDOW` 调至 16 MiB 重测并在验证记录注明**
- 重复读/二次打开：对比宽限表效果

**Step 4: 文档同步**（R9 教训——文档跟着代码走）：
- README.md / README.zh.md 中"adaptive prefetch/bandwidth estimation"描述改为"4 MiB 锚定窗口 + 句柄宽限表"
- CHANGELOG.md `[Unreleased]` 记录 Fixed（416 回退、206 校验）与 Changed（读模型重构）

**Step 5: 合并收口**
```bash
cd /e/GitHub/rs-f4ss && git merge --no-ff feat/read-window-model
# 最后一跑确认，然后按用户指示决定是否 push
```

---

## 风险与未覆盖点

1. **顺序吞吐回退风险**（D3 取舍）：4 MiB 串行窗在 高延迟×大带宽 链路弱于 16 MiB 预取；Task 7 Step 3 的调参步骤是明确出口。
2. **Linux 路径同受影响**：FuseAdapter::read 为双平台共享，WSL 全量 e2e 是硬性闸门，不可跳过。
3. **写路径未在本计划范围**：staging 落盘 + 后台上传队列（rs-cloudfs 第三支柱）是后续独立计划；本计划完成后 `file_info_timeout` 已与写性能解耦，但整文件 PUT 模型仍在。
4. **宽限表的 size 见证依赖 attr cache**：60 s TTL 内服务端改文件且大小恰好不变时，5 s 宽限窗内可能服务旧窗口——影响窗口 ≤5 s，与 rclone 同款限制，接受并记录。
5. **`send_with_retry_timeout` 超时行为无专门测试**（测超时需 sleep 型假服务器，性价比低）：机械参数化由既有 503-retry 测试守护，计划内注明。

## 验证记录（执行时填写）

| 日期 | 项目 | 基线 | 结果 |
|------|------|------|------|
| | 尾读 EOF-4MiB | ~5.4 s | |
| | small.txt ×100 | 155-195 ms | |
| | 顺序 256MiB 第 1/2 遍 | 209/211 MB/s | |
| | e2e.ps1 | 51/51 ×3 | |
| | WSL 三套 e2e | 51+55+40 | |
