# Implementation Tasks

Task breakdown for rs-f4ss.

**TDD Rule**: Every task starts with a failing test (RED), then implement (GREEN), then refactor.
**Status convention**: `[x]` = completed, `[ ]` = not started, `[~]` = in progress

---

## Phase 1 — WebDAV + FUSE/WinFsp Mount + CLI (COMPLETED)

**Status**: ✅ Complete
**Date**: 2026-05-31 ~ 2026-06-02
**Tests**: 97 unit + 51 E2E

### M1 — Project Skeleton + WebDAV Client

- [x] **T1.1** Workspace skeleton — `cargo build` succeeds
- [x] **T1.2** Error types — `BackendError` + `MountError` with thiserror
- [x] **T1.3** StorageBackend trait + Entry type
- [x] **T1.4** WebDAV URL construction — path traversal rejection, encoding
- [x] **T1.5** PROPFIND XML parsing — namespace-agnostic (local_name)
- [x] **T1.6** WebDavBackend struct + constructor + auth

### M2 — FUSE/WinFsp Read Operations

- [x] **T2.1** File handle table — allocate/release/write_at
- [x] **T2.2** Cache layer — moka-backed attrs + children
- [x] **T2.3** FuseAdapter: getattr + readdir — cache-first, backend-fallback
- [x] **T2.4** FuseAdapter: open + read + release — handle-based I/O

### M3 — FUSE/WinFsp Write Operations

- [x] **T3.1** FuseAdapter: write + flush + restore_dirty
- [x] **T3.2** FuseAdapter: mkdir + rmdir + unlink + rename
- [x] **T3.3** Error mapping: BackendError → errno / FspError

### M4 — Integration + CLI + Cross-Platform

- [x] **T4.1** MountEngine orchestration — platform dispatch
- [x] **T4.2** CLI argument parsing — clap derive
- [x] **T4.3** resolve_backend — URL scheme → backend factory
- [x] **T4.4** Cross-platform — Linux fuser + Windows WinFsp
- [x] **T4.5** E2E tests — bash (4 phases, 51 tests) + PowerShell (51 tests)
- [x] **T4.6** Code review fixes — P0 data loss, P1 cache coherence, P2/P3 quality

### Phase 1 Review

| Milestone | Tasks | Actual Tests |
|-----------|-------|-------------|
| M1: Skeleton + WebDAV | 6 | 27 |
| M2: FUSE Read | 4 | 23 |
| M3: FUSE Write | 3 | 9 |
| M4: Integration + CLI | 6 | 20 + 51 E2E |
| **Total** | **19 tasks** | **97 unit + 51 E2E** |

---

## Phase 2 — Multi-Protocol + REST API + Desktop App (COMPLETED)

**Status**: ✅ Complete
**Date**: 2026-06-02 ~ 2026-06-05
**Tests**: 164 unit + 51 E2E + 43 E2E API

### M5 — Feature Flag Infrastructure (COMPLETED)

- [x] **T5.1** Add feature definitions to `Cargo.toml` (webdav, http, api)
- [x] **T5.2** Add `#[cfg(feature)]` gates to backend modules and `lib.rs`
- [x] **T5.3** Refactor dependencies: move reqwest/quick-xml/base64 behind features
- [x] **T5.4** Verify feature combinations compile (webdav-only, http-only, all)
- [x] **T5.5** Run full test suite with all features
- [x] **T5.6** Extract `HttpClient` to `backend/common.rs` — eliminate ~120 lines duplication

### M6 — HTTP Static File Backend (COMPLETED)

- [x] **T6.1** `HttpBackend::from_url` — `static://` scheme → `http://` conversion
- [x] **T6.2** `parse_autoindex` — case-insensitive HTML tag matching (nginx/Apache/Caddy/Python)
- [x] **T6.3** Single-quote href support + HTML entity decoding in link text
- [x] **T6.4** `stat()` — HEAD for files, HEAD→GET fallback for directories
- [x] **T6.5** `read()` — Range request with full-download fallback
- [x] **T6.6** Write ops (PUT/MKCOL/DELETE/MOVE) — read_only guard
- [x] **T6.7** CLI integration: `static://host:9000` URL scheme routing
- [x] **T6.8** API integration: `create_backend` supports HTTP backend
- [x] **T6.9** Date parsing: nginx DD-Mon-YYYY + Apache YYYY-MM-DD + Caddy
- [x] **T6.10** Autoindex parsing tests — 18 unit tests

### M7 — MountManager + REST API (COMPLETED)

- [x] **T7.1** MountManager with DashMap — add/update/remove/list/get
- [x] **T7.2** MountHandle — unmount_slot + thread JoinHandle + AtomicBool cancel
- [x] **T7.3** REST API routes (axum) — 9 endpoints
- [x] **T7.4** CLI `serve` subcommand — `rs-f4ss serve --addr 0.0.0.0:8080`
- [x] **T7.5** Config persistence — JSON file, restore on startup
- [x] **T7.6** E2E API tests — `e2e-api.sh` (43 tests, 5 phases)

### M8 — Embedded Web UI (COMPLETED)

- [x] **T8.1** Vue 3 single-file HTML (ui.html)
- [x] **T8.2** Mount CRUD UI — create/edit/delete with form sections
- [x] **T8.3** Mount lifecycle UI — start/stop with status badges
- [x] **T8.4** CSS design system — CSS variables, modal, responsive layout
- [x] **T8.5** `include_str!` embedding — zero external dependencies

### M9 — Tauri Desktop App (COMPLETED)

- [x] **T9.1** Tauri v2 workspace setup with `rs-f4ss-desktop` crate
- [x] **T9.2** Single instance plugin — prevent multiple desktop processes
- [x] **T9.3** System tray — "显示"/"退出" menu, left-click shows window
- [x] **T9.4** 10 Tauri commands — health/version/list/get/create/update/delete/start/stop/restore
- [x] **T9.5** Inlined Vue 3 UI with `window.__TAURI__.core.invoke()`
- [x] **T9.6** Window close → hide to tray (not quit)
- [x] **T9.7** Cross-compile: Linux amd64 + Windows amd64 (via cargo-xwin)

### M10 — Linux Daemon Mode + CLI Management (COMPLETED)

- [x] **T10.1** Daemonize via `libc::fork()` + `setsid()` — no `-f` flag → background
- [x] **T10.2** PID file + log file in `$XDG_STATE_DIR/rs-f4ss/`
- [x] **T10.3** CLI management commands: list/add/del/start/stop (via REST API)
- [x] **T10.4** Tracing init after fork — avoid double-init panic
- [x] **T10.5** `fusermount -u` unmount triggers graceful shutdown

### M11 — Performance & Stability Optimization (COMPLETED)

- [x] **T11.1** FUSE kernel cache TTL: attr_valid/entry_valid 1s → 60s
- [x] **T11.2** FUSE `MaxReadahead(1MB)` + `FUSE_ASYNC_READ` + `FUSE_READDIRPLUS_AUTO`
- [x] **T11.3** `FOPEN_KEEP_CACHE` in `open()` — reuse kernel page cache when file unchanged
- [x] **T11.4** Dirty read fix — `read()` checks dirty write buffer first
- [x] **T11.5** `fsync` FUSE callback — flush dirty data on demand
- [x] **T11.6** `destroy` flush — write back all dirty handles on unmount
- [x] **T11.7** readdirplus optimization — static attr for "." and "..", no extra stat
- [x] **T11.8** getattr optimization — no HEAD fallback for size=0 from cache
- [x] **T11.9** Adaptive prefetch with bandwidth estimation and sequential pattern detection
- [x] **T11.10** Handle reclaim in `create()` on backend write failure
- [x] **T11.11** Default cache TTL 5s → 60s across CLI, core, and desktop

### Phase 2 Status Summary

| Milestone | Status | Description |
|-----------|--------|-------------|
| M5: Feature Flags | ✅ Complete | webdav/http/api features, HttpClient shared module |
| M6: HTTP Backend | ✅ Complete | nginx/Apache/Caddy/Python autoindex parsing |
| M7: REST API | ✅ Complete | MountManager + 9 endpoints + config persistence |
| M8: Web UI | ✅ Complete | Vue 3 embedded UI, CRUD + lifecycle |
| M9: Desktop App | ✅ Complete | Tauri v2 + system tray + single instance |
| M10: Daemon + CLI | ✅ Complete | Linux daemon mode + API management commands |
| M11: Performance | ✅ Complete | FUSE kernel cache, readahead, KEEP_CACHE, readdirplus |
| M12: File Sharing Server | ✅ Complete | HTTP + WebDAV server (serve feature), P2P sharing |

---

## Phase 2B — File Sharing Server (COMPLETED)

**Status**: ✅ Complete
**Date**: 2026-06-05
**Tests**: 187 unit (20 new)

### M12 — HTTP + WebDAV File Sharing Server (COMPLETED)

- [x] **T12.1** Feature flag definitions — `serve` feature in core + CLI Cargo.toml
- [x] **T12.2** server/mod.rs — FileServerState + resolve_path + auth + MIME map + Range parser + streaming utilities
- [x] **T12.3** server/autoindex.rs — nginx-format HTML generation + round-trip test with parse_autoindex()
- [x] **T12.4** server/handlers.rs — GET/HEAD (file + directory + Range) + PUT/DELETE/MOVE/MKCOL/COPY/OPTIONS
- [x] **T12.5** server/webdav.rs — PROPFIND (Depth 0/1) XML + PROPPATCH/LOCK stubs
- [x] **T12.6** CLI `share` subcommand — `rs-f4ss share serve /path --listen :8080`
- [x] **T12.7** Unit tests — resolve_path, parse_range, content_type, auth, autoindex round-trip
- [x] **T12.8** Documentation update — CLAUDE.md, TASKS.md, ADR.md

---

## Phase 3 — S3 Backend + Additional Protocols (FUTURE)

**Status**: 🔜 Future
**Depends on**: Phase 2 (completed)

### M12 — S3/MinIO Backend (NOT STARTED)

- [ ] **T12.1** AWS V4 signature implementation (HMAC-SHA256)
- [ ] **T12.2** S3Backend constructor + auth
- [ ] **T12.3** `list()` — ListObjectsV2 + XML parsing
- [ ] **T12.4** `stat()` — HeadObject
- [ ] **T12.5** `read()` — GetObject + Range
- [ ] **T12.6** `write()` — PutObject
- [ ] **T12.7** `mkdir()` / `delete()` / `rename()` (Copy+Delete)
- [ ] **T12.8** CLI: `s3://bucket` URL scheme routing
- [ ] **T12.9** MinIO compatibility testing

### M13 — Additional Backends (NOT STARTED)

- [ ] macOS support (mount_macos.rs + macFUSE)
- [ ] 百度网盘 backend
- [ ] FTP backend
- [ ] SFTP backend
- [ ] WebDAV Server aggregator

---

## Code Review History

### Review 1 (2026-06-01): P0-P3 Code Quality
- P0: Data loss fix (flush restore_dirty), Range chunked reads, readdirplus N+1
- P1: Cache coherence (FUSE TTL, invalidate_parent, rename invalidation)
- P2: XML namespace stripping, Windows unmount optimization
- P3: Entry rename, bytes::Bytes retry

### Review 2 (2026-06-02): API + Manager Review
- MountManager TOCTOU race fix, update_mount field semantics
- API unwrap safety, backend connectivity check
- Extract detect_protocol + from_url to eliminate duplication

### Review 3 (2026-06-02): HTTP Backend Review
- HttpClient extraction, case-insensitive HTML parsing
- Single-quote href, HTML entity decoding, stat() fallback

### Review 4 (2026-06-04): Performance & Stability Review (3-axis)
- C3: WNOHANG constant fix (0x40000000 → libc::WNOHANG)
- C2: open() removes extra stat, uses moka cache only for KEEP_CACHE
- C1: open() simplified — no stale handle risk from stat failure
- I1: Removed unused FromRawFd import
- I4: create() handle reclaim on backend write failure

### Review 5 (2026-06-05): Performance Optimization Review (2-axis)
- P0: readdirplus eliminates extra getattr for "." and ".." entries
- P1: getattr removes HEAD fallback for size=0 cached files

---

## Current Metrics

| Metric | Value |
|--------|-------|
| Source lines | ~9,600 |
| Unit tests | 187 |
| E2E bash (Linux) | 51 |
| E2E PowerShell (Windows) | 51 |
| E2E API | 43 |
| Supported protocols | WebDAV, HTTP static |
| Supported platforms | Linux (FUSE), Windows (WinFsp) |
| Feature flags | `webdav`, `http`, `api`, `serve` |
| REST API endpoints | 9 |
| File sharing methods | GET/HEAD/PUT/DELETE/MOVE/MKCOL/COPY/PROPFIND/LOCK |
| Binary sizes | CLI: 6.9M (Linux), 7.5M (Win); Desktop: 10.2M (Linux), 11.4M (Win) |

---

*Maintained by viccom. Last updated 2026-06-05.*
