# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

```bash
cargo build --release              # Build release binary
mkdir -p release && cp target/release/rs-f4ss release/  # Copy release artifact to release/
cargo test --all-features          # All unit tests (205 tests)
cargo test --no-default-features --features webdav  # WebDAV only
cargo test --no-default-features --features http    # HTTP only
cargo test --lib -- cache          # Run tests matching "cache" in lib crates
cargo fmt --all -- --check         # Check formatting
cargo clippy --all-targets --all-features -- -W clippy::all  # Lint (must be zero warnings)
bash tests/e2e.sh                  # E2E tests — requires dufs + /dev/fuse + release binary
bash tests/e2e-api.sh              # E2E API tests — requires running dufs + /dev/fuse
```

Run a single test: `cargo test test_name -- --nocapture`

Manual smoke test:
```bash
dufs /tmp/test-files -p 9000 -A
cargo run --bin rs-f4ss-cli -- http://localhost:9000 /mnt/test -f
RUST_LOG=rs_f4ss=debug  # enable debug logging
```

### Feature-Gated Builds

```bash
# Minimal (WebDAV only)
cargo build --no-default-features --features webdav

# HTTP static file server backend
cargo build --no-default-features --features http

# With REST API management + Web UI
cargo build --features webdav,api

# File sharing server only (no client, no API)
cargo build --no-default-features --features serve

# Everything
cargo build --all-features
```

### Cross-compilation (Windows from Linux)

```bash
# CLI: uses cargo-xwin with MSVC target
cargo xwin build --release -p rs-f4ss-cli --all-features --target x86_64-pc-windows-msvc

# Desktop: Tauri + WinFsp
cargo xwin build --release -p rs-f4ss-desktop --target x86_64-pc-windows-msvc
```

### Windows Runtime Requirements

- **WinFsp** must be installed: https://winfsp.dev
- The `winfsp` crate (v0.12) provides Rust bindings

## Architecture

This is a Rust workspace with three crates: `rs-f4ss-core` (library), `rs-f4ss-cli` (binary), and `rs-f4ss-desktop` (Tauri app).

### Platform Architecture

```
                    ┌─ Linux ──────────────────────────────────────┐
                    │  mount_linux.rs: fuser::Filesystem impl       │
                    │  Direct backend/cache/inodes operations       │
                    │  via block_on()                               │
FuseAdapter ────────┤                                               │
(platform-agnostic  │  Data: inodes (INodeNo → Path)               │
 async methods +    ├─ Windows ────────────────────────────────────┤
 cache/handles)     │  mount_windows.rs: WinFspAdapter              │
                    │  impl winfsp::FileSystemContext               │
                    │  Calls FuseAdapter async methods (path-based) │
                    └──────────────────────────────────────────────┘
                              │
                     StorageBackend trait (async)
                              │
                    ┌─────────┼──────────┐
                    │         │          │
              WebDavBackend  HttpBackend S3Backend
               (webdav)      (http)     (s3, planned)
```

### File Structure

```
crates/rs-f4ss-core/src/
├── mount.rs              FuseAdapter + MountEngine (platform-agnostic)
├── mount_linux.rs        fuser::Filesystem impl (#[cfg(target_os = "linux")])
├── mount_windows.rs      WinFspAdapter + FileSystemContext impl (#[cfg(target_os = "windows")])
├── error.rs              BackendError + MountError (platform-agnostic)
├── cache.rs              CacheLayer (moka-based, platform-agnostic)
├── handle.rs             HandleTable (platform-agnostic)
├── inode.rs              InodeMap (Linux uses, Windows skips)
├── prefetch.rs           BandwidthEstimator + ReadPattern (sequential detection)
├── persistence.rs        Mount config JSON persistence (feature = "api")
├── lib.rs                cfg-gated module declarations
├── manager.rs            MountManager (feature = "api")
├── api.rs                REST API routes + embedded Web UI (feature = "api")
├── ui.html               Vue 3 single-file management UI (include_str!)
├── server/               HTTP + WebDAV file sharing server (feature = "serve")
│   ├── mod.rs            FileServerState + create_router + resolve_path + auth + utilities
│   ├── handlers.rs       HTTP handlers: GET/HEAD/PUT/DELETE/MOVE/MKCOL/COPY/OPTIONS
│   ├── webdav.rs         WebDAV handlers: PROPFIND/PROPPATCH/LOCK
│   └── autoindex.rs      Nginx-format autoindex HTML generation
├── backend/
│   ├── mod.rs            StorageBackend trait + Box delegation + detect_protocol
│   ├── common.rs         HttpClient — shared URL build, retry, auth, read_full
│   ├── types.rs          Entry struct
│   ├── webdav.rs         WebDAV backend (feature = "webdav")
│   └── http.rs           HTTP static file backend (feature = "http")

crates/rs-f4ss-cli/src/
├── main.rs               CLI definition + daemon mode + API management commands
├── os_linux.rs            fusermount / /proc/mounts / ctrlc / daemonize
├── os_windows.rs          WinFsp unmount / status / ctrlc

desktop/src-tauri/src/
├── lib.rs                Tauri Builder + single instance + tray + window management
├── main.rs               Tauri entry point
├── commands.rs           10 #[tauri::command] functions delegating to MountManager
├── tray.rs               System tray with "显示"/"退出" menu
```

### Key Design Decisions

- **No VirtualFs trait**: Linux (inode-based) and Windows (path-based) have fundamentally different FS models. Each platform directly calls FuseAdapter's async methods — no intermediate abstraction layer.

- **Platform dispatch**: `MountEngine::mount()` uses `#[cfg(target_os)]` to call `mount_linux()` or `mount_windows()`. Unsupported platforms get a compile-time error.

- **cfg-gated dependencies**: `fuser` is only compiled on Linux, `winfsp` only on Windows. Core code (`mount.rs`, `error.rs`, `cache.rs`, etc.) compiles on all platforms.

- **Feature flags**: Protocol backends and service capabilities are feature-gated. `default = ["webdav"]` preserves backward compatibility. `http` for static file servers, `api` for REST API + Web UI. `serve` for HTTP + WebDAV file sharing server (enables peer-to-peer file sharing between rs-f4ss instances). `selfupdate` for the in-process self-updater built on top of the `rs-selfupdater` library.

- **Shared HttpClient**: `backend/common.rs` provides `HttpClient` with `build_url`, `send_with_retry`, `read_full_and_slice`, and `build_auth_header`. Both WebDAV and HTTP backends embed it, eliminating ~120 lines duplication.

- **Sync→Async bridge**: `FuseAdapter` owns a private `tokio::Runtime` (4 threads). Linux FUSE callbacks call `self.block_on()`. Windows WinFsp callbacks similarly bridge via `self.inner.block_on()`.

- **Write buffering**: Writes accumulate in `HandleTable` (buffer grows via `write_at`). Data is uploaded to backend on `flush` or `release` (fallback). Full PUT upload — no partial writes. `restore_dirty` preserves data on flush failure. Buffer capped at 2 GiB.

- **Inode mapping (Linux)**: `InodeMap` uses FNV-1a hash of path+kind to generate stable inode numbers. Collision falls back to sequential allocation. `rename_subtree` migrates all descendant mappings.

- **Cache**: `CacheLayer` wraps two `moka::future::Cache` instances (attrs + children) with configurable TTL (default 60s). Writes invalidate path + parent directory. FUSE kernel attr_valid/entry_valid = 60s.

- **Error mapping**: Linux: `map_backend_error()` in `mount_linux.rs` → `fuser::Errno`. Platform-agnostic: `map_to_io_error()` in `error.rs` → `std::io::Error`.

- **Read strategy**: `read()` checks dirty write buffer first, then per-handle read cache, then prefetch, then backend. Backend uses HTTP Range header, falls back to full download on 416/200. Adaptive prefetch based on bandwidth estimation and sequential pattern detection.

- **HTTP autoindex parsing**: Case-insensitive tag matching via `to_ascii_lowercase()`. Supports nginx/Apache/Caddy/Python formats. Handles single-quote href, HTML entities, multiple date formats.

- **FUSE kernel optimizations (Linux)**: `MaxReadahead(1MB)`, `FUSE_ASYNC_READ`, `FUSE_READDIRPLUS_AUTO` in `init()`. `FOPEN_KEEP_CACHE` in `open()` when cached attr exists. `attr_valid/entry_valid = 60s` to reduce callback frequency.

- **Daemon mode (Linux)**: Without `-f` flag, forks to background via `libc::fork()` + `setsid()`. PID file and log file stored in `$XDG_STATE_DIR/rs-f4ss/`. CLI provides `list/add/del/start/stop` commands to manage mounts via REST API.

- **File sharing server (`serve` feature)**: Serves local directories over HTTP (nginx autoindex) and WebDAV (PROPFIND). Output formats match `HttpBackend` and `WebDavBackend` client parsers for P2P round-trip. Zero coupling with mount/client code — `FileServerState` is self-contained. Supports Range requests (single range), Basic Auth, directory redirect, upload with 2GB limit. WebDAV: PROPFIND (Depth 0/1), MKCOL, MOVE, COPY, LOCK (pseudo), PROPPATCH (stub). CLI: `rs-f4ss share serve /path --listen :8080`.

- **Self-updater (`selfupdate` feature)**: Thin wrapper around the standalone `rs-selfupdater` crate. `SelfUpdater` in `crates/rs-f4ss-core/src/selfupdate/mod.rs` owns an `Arc<selfupdater::Updater>` and is cheap to clone. Manifest format is `latest.json` (`{version, date, assets: {os/arch: {url, sha256, size, signature?}}}`); SHA256 is mandatory, Ed25519 (Minisign) verification is opt-in via `RS_F4SS_UPDATE_PUBKEY`. CLI: `rs-f4ss update version|check|apply [--no-restart]`. REST: `GET /api/update/version|check|progress`, `POST /api/update/apply|restart`. The blocking `check()` / `apply()` calls are wrapped in `tokio::task::spawn_blocking` to keep the runtime responsive. Apply uses `self_replace::self_replace` (Unix `rename`, Windows `MoveFileEx`); restart uses Unix `execv` (PID preserved) or Windows `CreateProcess`. CI's `release.yml` job generates `latest.json` from the bare-binary artifacts.

- **Config persistence**: Mount configs saved to JSON file, restored on startup.

### Adding a New Backend

1. Create `crates/rs-f4ss-core/src/backend/newproto.rs`
2. Implement `StorageBackend` trait (8 async methods + protocol/server_addr)
3. Embed `HttpClient` from `common.rs` for URL build, retry, auth
4. Register in `backend/mod.rs` with `#[cfg(feature = "newproto")]`
5. Add feature definition in `Cargo.toml`
6. Add URL scheme to `detect_protocol()` in `mod.rs`
7. Add URL scheme to `resolve_backend()` in `main.rs` and `create_backend()` in `api.rs`
8. Use `MockBackend` pattern from `mount.rs` tests for integration testing

### Adding a New Platform

1. Create `crates/rs-f4ss-core/src/mount_<platform>.rs`
2. Implement the platform's FS callback trait, calling FuseAdapter async methods
3. Add `pub fn mount_<platform>()` function
4. Add cfg module in `lib.rs` and cfg dispatch in `mount.rs`
5. Create `crates/rs-f4ss-cli/src/os_<platform>.rs` for CLI helpers
6. Add cfg dispatch in `main.rs` and platform dependency in `Cargo.toml`

## Project Roadmap

| Phase | Status | Description |
|-------|--------|-------------|
| Phase 1 | ✅ Complete | WebDAV + FUSE/WinFsp + CLI |
| Phase 2 | ✅ Complete | HTTP backend, REST API, Web UI, Desktop app, Daemon mode |
| Phase 3 | 🔜 Future | S3 backend, macOS, 百度网盘/FTP/SFTP |

See `docs/TASKS.md` for detailed task breakdown and completion status.

## Code Stats

| Metric | Value |
|--------|-------|
| Source lines (core + cli + desktop) | ~8,772 |
| Unit tests | 164 |
| E2E bash (Linux) | 51 |
| E2E PowerShell (Windows) | 51 |
| E2E API | 43 |
| Supported protocols | WebDAV, HTTP static |
| Supported platforms | Linux (FUSE), Windows (WinFsp) |
| Feature flags | `webdav`, `http`, `api`, `serve`, `selfupdate` |
| REST API endpoints | 9 (14 with `selfupdate`) |
| Binary size (Linux CLI, stripped) | 6.9 MB |
