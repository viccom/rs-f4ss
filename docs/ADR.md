# Architecture Decision Records (ADR)

This document records key architectural decisions for rs-f4ss.

Format: **ADR-NNN** — Title
- **Status**: Proposed | Accepted | Deprecated | Superseded
- **Context**: What is the issue that we're seeing that is motivating this decision?
- **Decision**: What is the change that we're proposing/making?
- **Alternatives**: What other choices were considered?
- **Consequences**: What becomes easier or harder because of this change?

---

## ADR-001 — FUSE Library: fuser (cberner/fuser)

- **Status**: Accepted
- **Date**: 2026-05-31

### Context

rs-f4ss needs a FUSE library to implement user-space filesystem. The project must support Linux in Phase 1, with macOS and Windows planned for Phase 3 (Tauri desktop app). The FUSE layer directly affects cross-platform capability and API stability.

### Decision

Use **fuser 0.17** ([cberner/fuser](https://github.com/cberner/fuser)) as the FUSE library.

### Alternatives Considered

| Library | Pros | Cons |
|---------|------|------|
| **fuser** (cberner/fuser) | Mature, well-maintained, type-safe `Errno`/`Generation` API, Linux + macOS + BSD | No Windows support (WinFsp needed separately) |
| **fuse3** (zargy/fuse3) | Pure Rust, async, Linux-only, mature | No macOS/Windows support; Phase 3 requires rewrite |
| **libc fuse** (C binding) | Most stable, kernel-tested | Unsafe FFI, manual memory management, not async |

### Consequences

- **Positive**: Mature library with strong community support; type-safe API (`Errno`, `Generation`, `Config`, `SessionACL`); works on Linux and macOS; active development
- **Negative**: No built-in Windows support (Phase 3 Windows would need WinFsp integration separately)
- **Risk mitigation**: Abstract FUSE layer behind `FuseAdapter` so library swap is localized; pin fuser version in Cargo.toml

---

## ADR-002 — Cache Library: moka over manual LRU

- **Status**: Accepted
- **Date**: 2026-05-30

### Context

FUSE `getattr` and `readdir` are called frequently. Every call triggers a PROPFIND HTTP request to the WebDAV server. A metadata cache is needed to reduce network round-trips.

### Decision

Use **moka 0.12** with async `future` feature for the metadata cache layer.

### Alternatives Considered

| Library | Pros | Cons |
|---------|------|------|
| **lru** crate | Simple, deterministic eviction (strict LRU) | No async support, no TTL, manual synchronization |
| **mini-moka** | Synchronous, no threads | No async, no TTL, not designed for concurrent access |
| **moka 0.12** | Async `.get_with()`, native TTL, concurrent access, background eviction | Non-strict LRU (approximate); background thread adds complexity; eviction timing not deterministic |
| **cacache** | Disk-backed, content-addressed | Designed for file caching, not metadata; overkill |

### Consequences

- **Positive**: Zero-boilerplate async cache with TTL; concurrent reads don't block each other; automatic eviction with size limit
- **Negative**: Non-strict LRU means eviction tests must account for approximate behavior (test with `invalidate()` instead of relying on eviction order); background eviction thread adds ~1MB memory overhead
- **Key constraint**: Tests for eviction should NOT rely on exact LRU order. Use `invalidate()` for deterministic tests, and test eviction separately with large batches (10x capacity).

---

## ADR-003 — Write Strategy: Full-File Upload over Chunked Transfer

- **Status**: Accepted
- **Date**: 2026-05-30

### Context

FUSE `write` can be called with arbitrary offsets and sizes. The underlying WebDAV protocol supports `PUT` (full upload) and `PATCH` with `X-Update-Range: append` (dufs-specific). Need to decide the write strategy.

### Decision

Phase 1 uses **full-file upload**: buffer all writes in memory, upload entire file on `flush`/`release`.

### Alternatives Considered

| Strategy | Pros | Cons |
|----------|------|------|
| **Full-file upload** | Simple; compatible with all WebDAV servers; no partial-write corruption | Large files consume memory; O(n) for small edits |
| **Append-only (PATCH)** | Efficient for sequential writes; dufs supports it | Only works with dufs; random writes still need full upload |
| **Chunked upload** | Memory-efficient; supports random writes | Complex state management; not standard WebDAV; requires server support |
| **Write-through** | No buffering; immediate consistency | Every `write()` call = HTTP PUT; terrible performance for small writes |

### Consequences

- **Positive**: Works with any WebDAV server (not just dufs); simple implementation (~50 lines); no partial-write state corruption risk
- **Negative**: Memory usage = largest open file; writing 1 byte to a 1GB file requires 1GB RAM buffer; no crash recovery (unflushed writes lost on crash)
- **Mitigation**: Document memory implications in CLI help; add `--max-write-buffer` flag in Phase 4 to cap memory usage
- **Future**: Phase 4 can add append-only optimization for dufs servers (detect via `Server: dufs` header)

---

## ADR-004 — Dispatch Model: Dynamic Dispatch for CLI, Optional Static for Library

- **Status**: Accepted
- **Date**: 2026-05-30

### Context

`MountEngine` needs to work with any `StorageBackend` implementation. The CLI resolves the backend from URL scheme at runtime (user types `rs-f4ss http://...` or `rs-f4ss sftp://...`). Library users may know the backend at compile time.

### Decision

- **CLI**: Use `Box<dyn StorageBackend>` (dynamic dispatch via `MountEngine`)
- **Library**: Provide `MountEngineGeneric<B: StorageBackend>` (static dispatch, zero-cost)

### Alternatives Considered

| Approach | Pros | Cons |
|----------|------|------|
| **`Box<dyn>` only** | Simple; one API | Virtual dispatch overhead on every FUSE operation (~10ns/call) |
| **Generics only** | Zero-cost abstraction | CLI needs `match` on every operation (can't store different backends) |
| **Enum dispatch** | No heap allocation; pattern-match dispatch | Must update enum for every new backend; not extensible by library users |
| **Hybrid (accepted)** | Best of both worlds: CLI flexibility + library performance | Two APIs to maintain; slightly more complex |

### Consequences

- **Positive**: CLI code is clean (`resolve_backend` returns `Box<dyn>`); library users get zero-cost abstraction
- **Negative**: Two `MountEngine` variants to maintain; FUSE adapter implementation must work with `dyn StorageBackend`
- **Performance note**: The ~10ns virtual dispatch overhead is negligible compared to network latency (~1-100ms per HTTP request)

---

## ADR-005 — Cache Scope: Metadata Only, No File Content Caching

- **Status**: Accepted
- **Date**: 2026-05-30

### Context

FUSE `read` is called frequently (every `cat`, every `dd`, every `grep`). File content could be cached to reduce GET requests. However, file content can be large (GBs), and cache coherence with remote servers is complex.

### Decision

Phase 1 caches **metadata only** (file attributes and directory listings). File content is always fetched from the backend.

### Alternatives Considered

| Strategy | Pros | Cons |
|----------|------|------|
| **Metadata only (accepted)** | Predictable memory; no coherence issues; simple | Every file read = network request |
| **Full content cache** | Fast repeated reads | Memory unbounded with large files; coherence hard (remote changes invisible); eviction complex |
| **Read-ahead prefetch** | Good sequential read performance | Complex heuristics; wasted bandwidth for random reads |
| **Hybrid (metadata + small files)** | Best for small file workloads | Still has coherence issues; "small" threshold is arbitrary |

### Consequences

- **Positive**: Memory usage bounded by `cache_size * sizeof(CachedEntry)` (~10KB per entry); no stale-data risk for file content; simple implementation
- **Negative**: Every `cat file` hits the network; no benefit for repeated reads of same file; large file sequential read performance depends on FUSE readahead + backend throughput
- **Future**: Phase 4 can add optional read-ahead for sequential access patterns (detect via read offset pattern)

---

## ADR-006 — Feature Flags for Modular Compilation

- **Status**: Accepted
- **Date**: 2026-06-02

### Context

Phase 2 adds S3 backend, REST API, and WebDAV Server. Not all deployments need all features. The baseline use case (`rs-f4ss http://host /mnt`) should not pay the binary size or compile-time cost of axum, HMAC, or S3 signing.

### Decision

Use Cargo feature flags with a layered design:

```
default = ["webdav"]                    # Backward-compatible

# Protocol backends (orthogonal)
webdav  = ["reqwest", "quick-xml", "base64", "chrono"]
s3     = ["reqwest", "quick-xml", "chrono", "hmac", "sha2", "hex"]

# Mount capabilities (platform-gated)
fuse-mount   = ["fuser"]               # Linux
winfsp-mount = ["winfsp"]              # Windows

# Service capabilities
api    = ["axum", "tower-http", "serde_json"]
server = ["axum", "tower-http", "quick-xml"]
```

### Alternatives Considered

| Approach | Pros | Cons |
|----------|------|------|
| **Feature flags** | Standard Rust pattern, fine-grained, zero-cost | Complex Cargo.toml, combinatorial testing |
| Separate crates per backend | Clean separation | Workspace bloat, shared code duplication |
| Runtime plugin loading | Dynamic | Not Rust-native, security concerns |

### Consequences

- **Positive**: Minimal binary for CLI-only use; users opt-in to protocols they need; CI can test feature combinations
- **Negative**: Every new feature adds cfg gates; must test key feature combinations (4-5 matrix)
- **Rule**: Non-feature-gated code must not depend on feature-gated dependencies

---

## ADR-007 — REST API for Dynamic Mount Management

- **Status**: Proposed
- **Date**: 2026-06-02

### Context

Single-process single-mount is insufficient for multi-user or NAS scenarios. Need to manage multiple mounts dynamically without restarting.

### Decision

Add `MountManager` + axum REST API. Each mount runs in its own `std::thread` (FUSE/WinFsp requires blocking). Manager holds `DashMap<MountId, MountHandle>` with cancel signals.

### Alternatives Considered

| Approach | Pros | Cons |
|----------|------|------|
| **In-process MountManager** | Low latency, shared process | Thread-per-mount, complexity |
| External orchestrator (systemd) | Simple process model | No dynamic API, OS-specific |
| FUSE multi-mount in single thread | Single thread | fuser doesn't support this |

### Consequences

- **Positive**: One process, N mounts; HTTP API for any frontend (CLI, Web, Tauri)
- **Negative**: Mount failures don't crash the API server (must be isolated); thread-per-mount resource usage

---

## ADR-008 — WebDAV Server as Protocol Aggregator

- **Status**: Proposed
- **Date**: 2026-06-02

### Context

With multiple backends (WebDAV, S3, future Baidu/FTP), users need a unified access point. Instead of N mounts, expose one WebDAV endpoint that routes to N backends.

### Decision

Implement WebDAV Server that maps `StorageBackend` trait methods to HTTP methods (PROPFIND→list/stat, GET→read, PUT→write, etc.). URL prefixes route to different backends.

### Alternatives Considered

| Approach | Pros | Cons |
|----------|------|------|
| **Embedded WebDAV server** | Any WebDAV client works; trait maps 1:1 | Must implement full WebDAV spec |
| Custom REST API | Simpler | Requires custom client; no existing tool compat |
| FUSE passthrough | Reuse existing mount | Still need per-backend mounts |

### Consequences

- **Positive**: Any WebDAV client (Windows Explorer, macOS Finder, rclone) can access aggregated backends; `StorageBackend` trait maps naturally to WebDAV methods
- **Negative**: Must handle WebDAV edge cases (locks, properties, 207 Multi-Status); S3 rename non-atomic limitation exposed

---

## ADR-009 — FUSE Kernel Cache TTL: 60s for Network FS

- **Status**: Accepted
- **Date**: 2026-06-04

### Context

FUSE `attr_valid` and `entry_valid` control how long the kernel caches file attributes and directory entries before re-querying userspace. The previous 1s TTL caused excessive `getattr`/`lookup` FUSE callbacks on every file manager interaction, creating noticeable latency when browsing directories with many files.

### Decision

Set FUSE kernel `attr_valid = entry_valid = 60s`, matching the moka application-layer cache TTL.

### Alternatives Considered

| TTL | Pros | Cons |
|-----|------|------|
| **1s** (previous) | Fresh data, strong coherence | Excessive callbacks, slow browsing |
| **60s** (accepted) | Fast browsing, minimal callbacks | External changes visible after up to 60s |
| **300s** (rclone) | Very few callbacks | Stale data for extended periods |

### Consequences

- **Positive**: Directory browsing feels like local disk; file manager icon/status queries served from kernel cache
- **Negative**: External changes (from other machines) may take up to 60s to appear; write operations still invalidate moka cache immediately
- **Mitigation**: Write path (`flush`, `setattr`, `mkdir`, `unlink`, `rename`) calls `cache.invalidate()` to keep application cache coherent; only FUSE kernel cache has the 60s delay

---

## ADR-010 — Tauri Desktop over Custom GUI

- **Status**: Accepted
- **Date**: 2026-06-04

### Context

rs-f4ss needs a desktop application with system tray support for Windows and Linux. Options include Tauri, Electron, or a custom GTK/Qt app.

### Decision

Use **Tauri v2** with inlined Vue 3 UI and `withGlobalTauri: true` for `window.__TAURI__` injection.

### Alternatives Considered

| Approach | Pros | Cons |
|----------|------|------|
| **Tauri v2** (accepted) | Small binary (~10MB), Rust backend, native system tray | WebView dependency, Tauri-specific APIs |
| **Electron** | Mature, full Node.js | Large binary (~100MB), high memory |
| **GTK/Qt** | Native feel | Complex, platform-specific, no web UI reuse |
| **CLI-only** | Simple | No tray, no GUI management |

### Consequences

- **Positive**: Reuses existing Vue 3 Web UI; single-instance plugin prevents duplicate processes; system tray with hide-to-tray on window close; cross-compile via cargo-xwin for Windows
- **Negative**: WebView2 runtime required on Windows; desktop binary ~4MB larger than CLI-only

---

## ADR-011 — Adaptive Prefetch with Bandwidth Estimation

- **Status**: Accepted
- **Date**: 2026-06-05

### Context

Network file reads benefit from prefetching sequential data. However, aggressive prefetch wastes bandwidth for random reads, and static prefetch sizes are suboptimal across varying network speeds.

### Decision

Implement adaptive prefetch: `BandwidthEstimator` (EMA of observed throughput) calculates prefetch size as `bandwidth × pipeline_seconds`. Sequential pattern detection via `ReadPattern` (2+ consecutive sequential reads triggers prefetch). First reads use small size for quick response.

### Alternatives Considered

| Strategy | Pros | Cons |
|----------|------|------|
| **Adaptive EMA** (accepted) | Auto-tunes to network speed | Needs warm-up observations |
| **Fixed 16MB** | Simple | Wastes bandwidth on slow networks; too small on fast ones |
| **No prefetch** | Zero waste | Poor sequential read performance |
| **Full-file cache** | Fast rereads | Unbounded memory |

### Consequences

- **Positive**: Sequential reads (video playback, large file copy) auto-tune prefetch to fill the pipeline; first read responds quickly without prefetch
- **Negative**: EMA needs 1-2 observations to converge; initial estimate (5 MB/s) may be off

---

## ADR-012 — File Sharing Server: HTTP + WebDAV

- **Status**: Accepted
- **Date**: 2026-06-05

### Context

rs-f4ss can mount remote file servers (WebDAV, HTTP static) as local filesystems. However, sharing a local directory with another machine requires running a separate file server (dufs, nginx). A single binary that can both serve and mount files enables peer-to-peer file sharing.

### Decision

Add `serve` feature flag implementing an embedded HTTP + WebDAV file server. The server generates nginx-format autoindex HTML (parseable by `HttpBackend`) and standard WebDAV PROPFIND XML (parseable by `WebDavBackend`), ensuring client-server round-trip compatibility. Zero coupling with mount/client code — `FileServerState` is fully self-contained.

### Alternatives Considered

| Approach | Pros | Cons |
|----------|------|------|
| **HTTP + WebDAV combined** (accepted) | Full protocol support, works with both client backends | More code than HTTP-only |
| **HTTP autoindex only** | Simpler | Fragile HTML parsing; no metadata accuracy |
| **WebDAV only** | Accurate metadata | No browser compatibility; no third-party autoindex tools |
| **External dufs dependency** | No implementation effort | Defeats single-binary goal; extra deployment complexity |

### Consequences

- **Positive**: Single binary P2P file sharing; output formats match own client parsers perfectly; zero coupling with mount layer; feature-gated so users can exclude it
- **Negative**: ~800 lines new code; binary size increases ~800KB with serve feature; no upload size enforcement beyond 2GB default; no concurrent write protection (same as dufs)
- **Key design**: File server module is completely independent from mount/client modules — no shared state, no circular dependencies. Can be built standalone with `--features serve` without any client features.

---

## ADR Index

| ADR | Title | Status | Date |
|-----|-------|--------|------|
| ADR-001 | FUSE Library: fuser (cberner/fuser) | Accepted | 2026-05-31 |
| ADR-002 | Cache Library: moka | Accepted | 2026-05-30 |
| ADR-003 | Write Strategy: Full-file upload | Accepted | 2026-05-30 |
| ADR-004 | Dispatch Model: Hybrid dynamic/static | Accepted | 2026-05-30 |
| ADR-005 | Cache Scope: Metadata only | Accepted | 2026-05-30 |
| ADR-006 | Feature Flags for Modular Compilation | Accepted | 2026-06-02 |
| ADR-007 | REST API for Dynamic Mount Management | Accepted | 2026-06-04 |
| ADR-008 | WebDAV Server as Protocol Aggregator | Proposed | 2026-06-02 |
| ADR-009 | FUSE Kernel Cache TTL: 60s | Accepted | 2026-06-04 |
| ADR-010 | Tauri Desktop over Custom GUI | Accepted | 2026-06-04 |
| ADR-011 | Adaptive Prefetch with Bandwidth Estimation | Accepted | 2026-06-05 |
| ADR-012 | File Sharing Server: HTTP + WebDAV | Accepted | 2026-06-05 |
