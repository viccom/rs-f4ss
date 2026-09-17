# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- GitHub Actions CI workflow (`.github/workflows/ci.yml`)
  - `rustfmt`, `clippy`, unit/integration tests across the feature matrix
  - Linux + Windows builds (FUSE / WinFsp)
  - E2E suite (FUSE, REST API, P2P share) with log artifact upload on failure
  - Windows E2E job (`tests/e2e.ps1`) and a daily `schedule:` trigger
- dependabot config (cargo + github-actions) and `SECURITY.md`

### Fixed
- `share serve`: header sanitisation for Content-Disposition (remote DoS),
  loopback default bind, half-specified `--user`/`--pass` rejected
- `serve`: refuse default credentials on a non-loopback bind; salted password
  hashing with constant-time verification
- `selfupdater`: backup + rollback around Windows self-replace
- `e2e-api.sh`: summary counter no longer exceeds the total

### Changed
- Self-update signature chain removed (Option B): updates are SHA-256-only
  over HTTPS; the unused unauthenticated update router was deleted
- README/ADR reconciled with code (env var names, config path, feature
  list, counts, superseded ADRs)

---

## [0.3.0] - 2026-06-11

### Added
- In-process self-updater (`selfupdater` crate vendored into the workspace;
  `rs-f4ss update check|apply`, REST `/api/update/*`)
- Release pipeline: v* tag triggered 4-target cross-build publishing
  tar.gz/zip assets and `latest.json`
- REST API: Basic Auth + Web UI login, share password hashing

### Fixed
- Fourth review pass: panic DoS via malformed requests, test inconsistencies,
  validation gaps, TOCTOU race in mount manager
- Windows: WinFsp DLL staging for tests, POSIX path handling on rename

### Changed
- `selfupdater` moved from a local path dependency into the workspace
- README split into English `README.md` + Chinese `README.zh.md`
- Version bumped 0.2.0 → 0.3.0

---

## [0.2.0] - 2026-06-07

### Added
- Multi-protocol backends: WebDAV + HTTP static file
- FUSE (Linux) + WinFsp (Windows) filesystem interfaces
- REST API + Vue 3 Web UI for remote mount management
- Desktop app (Tauri): system tray + GUI
- P2P file sharing service (HTTP + WebDAV), interoperable with the rs-f4ss client
- Basic Auth across REST / UI / Share
- Config persistence: mount configs stored as JSON, auto-restored on start
- Code review passes: security review (cfg gates, persistence, TOCTOU, UI
  credentials), localStorage session persistence fix, 10-issue review fix

### Changed
- Version bumped 0.1.0 → 0.2.0

---

## [0.1.0] - 2026-06-06

### Added
- Core library (`rs-f4ss-core`)
  - `StorageBackend` trait for pluggable protocols
  - `WebDavBackend` implementation
  - `MountEngine` with FUSE integration via fuser (cberner/fuser)
  - Metadata LRU cache (configurable TTL)
  - File handle table
  - Error types with HTTP→errno mapping
  - Event system for monitoring
- CLI frontend (`rs-f4ss-cli`)
  - `rs-f4ss <url> <mountpoint>` command
  - Authentication (`--user`, `--pass`)
  - Read-only mode (`--read-only`)
  - Cache configuration (`--cache-ttl`, `--cache-size`)
  - Foreground mode (`--foreground`)
  - `status` and `unmount` subcommands
- WebDAV operations
  - PROPFIND (stat, list)
  - GET with Range (read)
  - PUT (write)
  - MKCOL (mkdir)
  - DELETE (unlink, rmdir)
  - MOVE (rename)
- Tests
  - Unit tests for all modules
  - Integration tests with MockBackend
  - E2E tests with real dufs server
- Project documentation: SPEC, TEST_PLAN, DEV_GUIDE, CONTRIBUTING, TDD docs
- Issue & PR templates

[Unreleased]: https://github.com/viccom/rs-f4ss/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/viccom/rs-f4ss/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/viccom/rs-f4ss/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/viccom/rs-f4ss/releases/tag/v0.2.0
