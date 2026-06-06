# rs-f4ss Development Guide

**Version**: 1.0.0
**Date**: 2026-06-02
**Phase 1 Status**: Complete (97 unit tests + 51 E2E)

---

## 1. Prerequisites

### 1.1 System Dependencies

#### Linux (Ubuntu/Debian)
```bash
sudo apt update
sudo apt install -y \
    fuse3 \
    libfuse3-dev \
    pkg-config \
    build-essential
```

#### Linux (Fedora)
```bash
sudo dnf install -y fuse3 fuse3-devel pkg-config gcc
```

#### macOS (Phase 3)
```bash
brew install macfuse
```

#### Windows (Phase 3)
```powershell
choco install winfsp
```

### 1.2 Rust Toolchain

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Required components
rustup component add clippy rustfmt

# Minimum supported Rust version
rustup show active-toolchain  # Should be 1.75+ (for async trait stabilization)
```

### 1.3 Test Server

```bash
# Install dufs (for E2E tests)
cargo install dufs

# Or build from source
cd /path/to/dufs && cargo build --release && cp target/release/dufs /usr/local/bin/
```

---

## 2. Project Setup

### 2.1 Clone and Build

```bash
git clone https://github.com/viccom/rs-f4ss.git
cd rs-f4ss

# Build all crates
cargo build

# Build with release optimizations
cargo build --release
```

### 2.2 Workspace Structure

```
rs-f4ss/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── rs-f4ss-core/    # Core library
│   ├── rs-f4ss-cli/     # CLI binary
│   ├── rs-f4ss-web/     # Web server (Phase 2)
│   └── rs-f4ss-app/     # Tauri app (Phase 3)
├── docs/                   # Documentation
└── tests/                  # Integration/E2E tests
```

---

## 3. Development Workflow

### 3.1 TDD Cycle

```bash
# 1. RED — Write a failing test
cargo test test_new_feature -- --nocapture
# Expected: FAIL

# 2. GREEN — Write minimal code to pass
# Edit src/.../module.rs
cargo test test_new_feature -- --nocapture
# Expected: PASS

# 3. REFACTOR — Clean up while keeping tests green
cargo test --lib
# Expected: ALL PASS

# 4. Lint and format
cargo clippy --all --all-targets -- -D warnings
cargo fmt --all
```

### 3.2 Running Tests

```bash
# All tests
cargo test --all

# Unit tests only (fast)
cargo test --lib

# Specific module
cargo test --lib cache

# Specific test
cargo test test_cache_ttl_expire -- --nocapture

# Integration tests (requires FUSE)
cargo test --test integration

# E2E tests (requires dufs server + FUSE)
cargo test --test e2e -- --test-threads=1

# With backtrace
RUST_BACKTRACE=1 cargo test --lib

# With logging
RUST_LOG=dufs_mount=debug cargo test -- --nocapture
```

### 3.3 Manual Testing

```bash
# Terminal 1: Start test dufs server
dufs /tmp/test-files -p 9000 --allow-all -A

# Terminal 2: Mount and test
cargo run --bin rs-f4ss-cli -- http://localhost:9000 /mnt/test --foreground

# Terminal 3: Test operations
ls /mnt/test/
cat /mnt/test/hello.txt
cp /etc/hosts /mnt/test/
mkdir /mnt/test/newdir/
rm /mnt/test/hosts

# Cleanup
fusermount -u /mnt/test
```

---

## 4. Code Quality

### 4.1 Clippy

```bash
# Run clippy (warnings are errors in CI)
cargo clippy --all --all-targets -- -D warnings

# Common fixes
cargo clippy --fix --all --all-targets
```

### 4.2 Formatting

```bash
# Check formatting
cargo fmt --all --check

# Auto-format
cargo fmt --all
```

### 4.3 Test Coverage

```bash
# Install tarpaulin
cargo install cargo-tarpaulin

# Generate coverage report
cargo tarpaulin --all --out Html --output-dir coverage/

# View report
open coverage/index.html
```

### 4.4 Documentation

```bash
# Build and open docs
cargo doc --all --open

# Check for broken links
cargo doc --all 2>&1 | grep -i "warning.*link"
```

---

## 5. Adding a New Backend

### 5.1 TDD Steps

```bash
# 1. Create backend module
touch crates/rs-f4ss-core/src/backend/newprotocol.rs

# 2. Write tests first
# Add to newprotocol.rs:
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_list_directory() {
        let backend = NewProtocolBackend::new(/* ... */);
        let entries = backend.list("/").await.unwrap();
        assert!(!entries.is_empty());
    }

    #[tokio::test]
    async fn test_read_file() {
        let backend = NewProtocolBackend::new(/* ... */);
        let data = backend.read("/test.txt", 0, 100).await.unwrap();
        assert_eq!(data, b"Hello, World!");
    }

    // ... more tests
}

# 3. Run tests (expect failures)
cargo test --lib backend::newprotocol

# 4. Implement StorageBackend trait
# 5. Run tests until green
# 6. Register in backend/mod.rs
# 7. Add URL scheme to CLI resolve_backend
```

### 5.2 Backend Checklist

- [ ] Implement all required `StorageBackend` methods
- [ ] Unit tests for XML/JSON parsing (if applicable)
- [ ] Unit tests for error handling
- [ ] Integration test with MockBackend
- [ ] E2E test with real server (if available)
- [ ] Add to CLI `resolve_backend` URL routing
- [ ] Add feature flag to `Cargo.toml`
- [ ] Update SPEC.md backend table
- [ ] Update README.md supported protocols

---

## 6. Debugging

### 6.1 Logging

```bash
# Enable debug logging
RUST_LOG=dufs_mount=debug cargo run --bin rs-f4ss-cli -- http://localhost:9000 /mnt/test -f

# Trace level (very verbose)
RUST_LOG=dufs_mount=trace cargo run --bin rs-f4ss-cli -- http://localhost:9000 /mnt/test -f

# Log to file
RUST_LOG=dufs_mount=debug cargo run --bin rs-f4ss-cli -- http://localhost:9000 /mnt/test -f 2>&1 | tee mount.log
```

### 6.2 FUSE Debugging

```bash
# Mount with FUSE debug output
fusermount -o debug /mnt/test

# Check mount status
mount | grep fuse

# Force unmount (if stuck)
fusermount -uz /mnt/test
```

### 6.3 WebDAV Debugging

```bash
# Test PROPFIND manually
curl -X PROPFIND http://localhost:9000/ -H "Depth: 1" -u admin:secret

# Test GET with Range
curl -v http://localhost:9000/test.txt -H "Range: bytes=0-99"

# Test PUT
curl -X PUT http://localhost:9000/new.txt -d "Hello, World!"
```

---

## 7. Release Process

### 7.1 Version Bump

```bash
# Update version in all Cargo.toml
# crates/rs-f4ss-core/Cargo.toml
# crates/rs-f4ss-cli/Cargo.toml

# Update CHANGELOG.md
# Update SPEC.md version header

# Commit
git add -A && git commit -m "chore: bump version to 0.1.0"
git tag v0.1.0
```

### 7.2 Build Release Binaries

```bash
# Linux x86_64
cargo build --release --target x86_64-unknown-linux-gnu

# Linux aarch64 (cross-compile)
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu

# Package
tar -czf rs-f4ss-linux-x86_64.tar.gz -C target/x86_64-unknown-linux-gnu/release rs-f4ss-cli
```

### 7.3 Publish to crates.io

```bash
cd crates/rs-f4ss-core
cargo publish --dry-run  # Check
cargo publish             # Publish
```

---

## 8. Common Issues

### 8.1 "fuse: device not found"

```bash
# Check FUSE is loaded
lsmod | grep fuse

# Load FUSE module
sudo modprobe fuse

# Add user to fuse group
sudo usermod -aG fuse $USER
# Log out and back in
```

### 8.2 "Transport endpoint is not connected"

```bash
# Mount process crashed. Force unmount:
fusermount -uz /mnt/test
```

### 8.3 "Permission denied" on mount

```bash
# Check /dev/fuse permissions
ls -la /dev/fuse

# Should be crw-rw-rw- or at least group-accessible
sudo chmod 666 /dev/fuse
# Or add user to fuse group (preferred)
```

---

*Dev Guide maintained by viccom. Last updated 2026-05-30.*
