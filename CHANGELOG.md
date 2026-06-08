# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- Project initialization
- Specification document (SPEC.md v0.3.0)
- Test plan (TEST_PLAN.md)
- Development guide (DEV_GUIDE.md)
- Contributing guide (CONTRIBUTING.md)
- README with quick start guide
- TDD methodology document ([docs/TDD.md](docs/TDD.md))
- GitHub Actions CI workflow (`.github/workflows/ci.yml`)
  - `rustfmt`, `clippy`, unit/integration tests across the feature matrix
  - Linux + Windows builds (FUSE / WinFsp)
  - E2E suite (FUSE, REST API, P2P share) with log artifact upload on failure
- Issue & PR templates (`.github/ISSUE_TEMPLATE/`, `.github/PULL_REQUEST_TEMPLATE.md`)

### Changed
- README rewritten to the standard of excellent GitHub open-source projects:
  feature highlights, architecture diagram, full usage, feature matrix, build
  options, configuration, roadmap, project stats, acknowledgments

---

## [0.1.0] - TBD

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

[Unreleased]: https://github.com/viccom/rs-f4ss/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/viccom/rs-f4ss/releases/tag/v0.1.0
