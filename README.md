# rs-f4ss

Multi-protocol remote filesystem mount client + P2P file sharing server.

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org/)
[![Tests](https://img.shields.io/badge/tests-205%20passed-green.svg)]()

## Features

- **Mount** remote WebDAV/HTTP servers as local filesystems via FUSE/WinFsp
- **Share** local directories over HTTP + WebDAV (P2P between rs-f4ss instances)
- **Manage** mounts and shares via REST API + Web UI
- **Desktop** Tauri app with system tray (Windows/Linux)

## Quick Start

```bash
# Build
git clone https://git.metme.top/viccom/rs-f4ss.git
cd rs-f4ss
cargo build --release

# Mount a remote WebDAV server
rs-f4ss http://192.168.1.100:5000 /mnt/remote

# Use like local filesystem
ls /mnt/remote/
cp file.txt /mnt/remote/
cat /mnt/remote/readme.md
```

## Usage

### Mount

```bash
# Basic
rs-f4ss http://server:5000 /mnt/remote

# With auth
rs-f4ss http://server:5000 /mnt/remote --user admin --pass secret

# Read-only, foreground
rs-f4ss http://server:5000 /mnt/remote --read-only -f

# Unmount
rs-f4ss unmount /mnt/remote
# or: fusermount -u /mnt/remote (Linux)

# Show active mounts
rs-f4ss status
```

### API Server

```bash
# Start management server (single instance only)
rs-f4ss serve --listen 0.0.0.0:8080

# Web UI: http://localhost:8080
```

### Mount Management (via API)

```bash
rs-f4ss mount list                              # List mount configs
rs-f4ss mount add myserver --url http://... --path /mnt/data
rs-f4ss mount start myserver                    # Start mount
rs-f4ss mount stop myserver                     # Stop mount
rs-f4ss mount del myserver                      # Delete config
```

### File Sharing

```bash
# Standalone file server (no API needed)
rs-f4ss share serve /path/to/dir --listen :8080
rs-f4ss share serve /path/to/dir --listen :8080 --user admin --pass secret
rs-f4ss share serve /path/to/dir --listen :8080 --read-only

# Managed via API (requires rs-f4ss serve running)
rs-f4ss share list
rs-f4ss share add myfiles --path /data --listen :8081
rs-f4ss share start myfiles
rs-f4ss share stop myfiles
rs-f4ss share del myfiles
```

## Protocols & Platforms

| Protocol | Status | Examples |
|----------|--------|----------|
| **WebDAV** | ✅ | dufs, Nginx, Apache, Nextcloud |
| **HTTP** static | ✅ | nginx autoindex, Caddy, Python http.server |
| S3 | 🔜 Planned | AWS S3, MinIO |
| SFTP/FTP | 🔜 Planned | OpenSSH, vsftpd |

| Platform | Status | Driver |
|----------|--------|--------|
| **Linux** | ✅ | `apt install fuse3` |
| **Windows** | ✅ | [WinFsp](https://winfsp.dev) |
| macOS | 🔜 Planned | macFUSE |

## Build Options

```bash
cargo build --release                                   # Default (WebDAV)
cargo build --no-default-features --features http       # HTTP static only
cargo build --features webdav,http,api                  # + REST API + Web UI
cargo build --features webdav,http,api,serve            # + File sharing server
cargo build --all-features                              # Everything
```

## Development

```bash
cargo test --all-features                   # 205 unit tests
cargo clippy --all-targets --all-features -- -W clippy::all
cargo fmt --all -- --check
bash tests/e2e.sh                           # E2E (requires dufs + /dev/fuse)
bash tests/e2e-api.sh                       # API E2E
bash tests/e2e-share.sh                     # P2P share E2E
```

## Architecture

```
                    ┌─ Linux ──────────────────────────────┐
                    │  fuser::Filesystem (FUSE)             │
FuseAdapter ────────┤                                        │
                    ├─ Windows ────────────────────────────┤
                    │  WinFspAdapter (WinFsp)               │
                    └──────────────────────────────────────┘
                              │
                     StorageBackend trait
                    ┌─────────┼──────────┐
              WebDavBackend  HttpBackend  S3Backend (planned)

         ┌──────────────────────────────────────┐
         │           rs-f4ss-core (library)      │
         │  CacheLayer · HandleTable · InodeMap  │
         │  MountManager · ShareManager          │
         │  REST API + Vue 3 Web UI             │
         │  HTTP/WebDAV File Server              │
         └──────────────────────────────────────┘
              │              │              │
         rs-f4ss-cli    rs-f4ss-desktop   rs-f4ss serve
          (CLI)         (Tauri app)      (API server)
```

## Documentation

| Document | Description |
|----------|-------------|
| [CLAUDE.md](CLAUDE.md) | Architecture, build commands, file structure |
| [SPEC.md](docs/SPEC.md) | Phase 1 specification |
| [SPEC-v2.md](docs/SPEC-v2.md) | Phase 2 expansion specification |
| [ADR.md](docs/ADR.md) | Architecture decision records |
| [TASKS.md](docs/TASKS.md) | Task breakdown and status |

## License

MIT
