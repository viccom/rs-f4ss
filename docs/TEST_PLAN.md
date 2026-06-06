# rs-f4ss Test Plan

**Version**: 1.0.0
**Date**: 2026-06-02
**Status**: Phase 1 Complete

---

## Test Progress

| Phase | Unit Tests | E2E Tests | Status |
|-------|-----------|-----------|--------|
| Phase 1 (WebDAV + FUSE) | 97 | 51 (4 phases) | ✅ Complete |
| Phase 2 (S3 + API + Server) | TBD | TBD | 📋 Planned |

---

## 1. Test Strategy Overview

```
┌─────────────────────────────────────────────────┐
│                Test Pyramid                      │
│                                                 │
│                    ╱╲                            │
│                   ╱E2E╲     ← Few, slow, high   │
│                  ╱──────╲      confidence        │
│                 ╱  Integ ╲   ← Moderate, medium  │
│                ╱──────────╲    speed             │
│               ╱   Unit    ╲ ← Many, fast, low   │
│              ╱──────────────╲  cost              │
└─────────────────────────────────────────────────┘

Phase 1 Target: Unit 80%+ | Integration 60%+ | E2E smoke
```

### 1.1 TDD Workflow

```
1. Write failing test (RED)
2. Write minimal code to pass (GREEN)
3. Refactor while keeping tests green (REFACTOR)
4. Repeat
```

Every feature starts with a test. No production code without a corresponding test.

### 1.2 Test Categories

| Category | Scope | Speed | When to Run |
|----------|-------|-------|-------------|
| **Unit** | Single function/struct | <1ms | Every save |
| **Integration** | Module interaction | <100ms | Every commit |
| **E2E** | Full system (dufs + mount) | <10s | Every PR |
| **Performance** | Throughput, latency | <60s | Weekly / release |

---

## 2. Unit Tests

### 2.1 StorageBackend Trait (`backend/mod.rs`)

| Test | Input | Expected | TDD Phase |
|------|-------|----------|-----------|
| `test_entry_is_dir_true` | `Entry { is_dir: true, .. }` | `entry.is_dir() == true` | RED→GREEN |
| `test_entry_is_dir_false` | `Entry { is_dir: false, .. }` | `entry.is_dir() == false` | RED→GREEN |
| `test_entry_to_file_attr` | `Entry { size: 100, mtime: now, .. }` | `FileAttr { size: 100, .. }` | RED→GREEN |
| `test_backend_error_not_found` | `BackendError::NotFound("x")` | Display: "Not found: x" | RED→GREEN |

### 2.2 WebDAV Backend (`backend/webdav.rs`)

| Test | Input | Expected | TDD Phase |
|------|-------|----------|-----------|
| `test_parse_propfind_single` | XML response (depth=0) | `Entry` with correct fields | RED→GREEN |
| `test_parse_propfind_directory` | XML with `<D:collection/>` | `is_dir: true` | RED→GREEN |
| `test_parse_propfind_file` | XML without collection | `is_dir: false` | RED→GREEN |
| `test_parse_propfind_empty_dir` | XML with 0 responses | `vec![]` | RED→GREEN |
| `test_parse_propfind_multiple` | XML with 3 responses | `vec![3 entries]` | RED→GREEN |
| `test_build_propfind_request` | path="/docs" | Correct XML body | RED→GREEN |
| `test_build_range_header` | offset=100, size=500 | `"Range: bytes=100-599"` | RED→GREEN |
| `test_url_encoding` | path="/docs/文件.txt" | Properly encoded URL | RED→GREEN |
| `test_url_traversal_rejected` | path="/../etc/passwd" | Error | RED→GREEN |
| `test_auth_header_basic` | user="admin", pass="secret" | `Authorization: Basic ...` | RED→GREEN |

### 2.3 Cache Layer (`cache.rs`)

| Test | Input | Expected | TDD Phase |
|------|-------|----------|-----------|
| `test_cache_set_get` | set("a", val) → get("a") | `Some(val)` | RED→GREEN |
| `test_cache_miss` | get("nonexistent") | `None` | RED→GREEN |
| `test_cache_ttl_expire` | set("a", val) → wait(ttl+1) → get("a") | `None` | RED→GREEN |
| `test_cache_eviction` | set 257 entries (max=256) → get oldest | `None` (evicted) | RED→GREEN |
| `test_cache_invalidate` | set("a", val) → invalidate("a") → get("a") | `None` | RED→GREEN |
| `test_cache_invalidate_parent` | set("/a/b", val) → invalidate_parent("/a/b/c") | "/a/b" invalidated | RED→GREEN |
| `test_cache_clear` | set multiple → clear → get any | `None` | RED→GREEN |

### 2.4 File Handle Table (`handle.rs`)

| Test | Input | Expected | TDD Phase |
|------|-------|----------|-----------|
| `test_allocate_handle` | allocate() | Unique u64, non-zero | RED→GREEN |
| `test_get_handle` | alloc → get(id) | `Some(state)` | RED→GREEN |
| `test_release_handle` | alloc → release → get | `None` | RED→GREEN |
| `test_multiple_handles` | alloc 3x | 3 distinct IDs | RED→GREEN |
| `test_write_buf_dirty` | alloc → write(buf) | `dirty == true` | RED→GREEN |

### 2.5 MountEngine (`mount.rs`)

| Test | Input | Expected | TDD Phase |
|------|-------|----------|-----------|
| `test_engine_new` | `MountEngine::new(mock_backend, config)` | Engine created | RED→GREEN |
| `test_engine_events_subscribe` | `engine.events()` | Returns receiver | RED→GREEN |
| `test_engine_status_initial` | `engine.status()` | `MountStatus::Idle` | RED→GREEN |

### 2.6 Error Mapping (`error.rs`)

| Test | Input | Expected | TDD Phase |
|------|-------|----------|-----------|
| `test_http_404_to_enoent` | `BackendError::NotFound` | `FsError` with ENOENT | RED→GREEN |
| `test_http_403_to_eacces` | `BackendError::PermissionDenied` | `FsError` with EACCES | RED→GREEN |
| `test_http_500_to_eio` | `BackendError::ConnectionFailed` | `FsError` with EIO | RED→GREEN |
| `test_read_only_to_erofs` | Write on read-only backend | `FsError` with EROFS | RED→GREEN |

### 2.7 Edge Cases & Boundary Conditions

| Test | Input | Expected | Priority | TDD Phase |
|------|-------|----------|----------|-----------|
| `test_empty_file_read` | `read("/empty.txt", 0, 100)` | `vec![]` (zero bytes) | High | RED→GREEN |
| `test_empty_file_write` | write `vec![]` to new file | File created with size=0 on server | High | RED→GREEN |
| `test_path_with_spaces` | `stat("/my file.txt")` | URL-encoded correctly, result returned | High | RED→GREEN |
| `test_path_with_hash` | `stat("/a#b.txt")` | `%23` encoded, not treated as URL fragment | High | RED→GREEN |
| `test_path_with_percent` | `stat("/100%.txt")` | `%25` double-encoded correctly | Medium | RED→GREEN |
| `test_path_with_question` | `stat("/a?b.txt")` | `%3F` encoded, not treated as query param | Medium | RED→GREEN |
| `test_path_with_unicode` | `stat("/中文/文件.txt")` | UTF-8 percent-encoded, round-trips correctly | High | RED→GREEN |
| `test_path_double_dot` | `stat("/a/../etc/passwd")` | Rejected or normalized (no traversal) | High | RED→GREEN |
| `test_path_null_byte` | `stat("/file\x00.txt")` | Rejected with EINVAL | High | RED→GREEN |
| `test_path_too_long` | `stat("/" + "a".repeat(4000))` | Rejected with ENAMETOOLONG | Medium | RED→GREEN |
| `test_propfind_self_entry` | PROPFIND depth=1, first response is directory itself | Self-entry filtered from results | High | RED→GREEN |
| `test_propfind_no_mtime` | XML without `<D:getlastmodified>` | `mtime == UNIX_EPOCH` | High | RED→GREEN |
| `test_propfind_malformed_xml` | Invalid XML response | `BackendError::ProtocolError` | Medium | RED→GREEN |
| `test_concurrent_getattr_same_file` | 10 parallel getattr on same path | All succeed, backend called ≤ N times (cache) | High | RED→GREEN |
| `test_concurrent_read_write` | Read + Write on same file simultaneously | No deadlock, no data corruption | Medium | RED→GREEN |
| `test_mountpoint_not_empty` | Mount to non-empty directory | OS returns error or warning | Medium | RED→GREEN |
| `test_large_directory_10k` | Directory with 10,000 files | readdir completes, no OOM | Medium | RED→GREEN |
| `test_backend_unreachable` | Backend URL points to nothing | `BackendError::ConnectionFailed`, FUSE returns EIO | High | RED→GREEN |
| `test_backend_timeout` | Backend takes >30s to respond | Request cancelled, EIO returned | Medium | RED→GREEN |
| `test_auth_incorrect` | Wrong username/password | 401 → EACCES | High | RED→GREEN |
| `test_write_exceeds_server_limit` | Write file larger than server allows | 413 → EFBIG | Low | RED→GREEN |

---

## 3. Integration Tests

### 3.1 Backend + Cache Integration

| Test | Scenario | Expected | TDD Phase |
|------|----------|----------|-----------|
| `test_cache_hit_on_second_stat` | stat("/a") → stat("/a") | Second call is cache hit (no HTTP) | RED→GREEN |
| `test_cache_invalidated_on_write` | stat("/a") → write("/a", data) → stat("/a") | Fresh data from server | RED→GREEN |
| `test_cache_invalidated_on_mkdir` | list("/dir") → mkdir("/dir/new") → list("/dir") | New entry visible | RED→GREEN |

### 3.2 FUSE + Backend Integration (Linux only)

| Test | Scenario | Expected | TDD Phase |
|------|----------|----------|-----------|
| `test_fuse_ls_mountpoint` | Mount → `ls /mnt/test/` | Files listed correctly | RED→GREEN |
| `test_fuse_cat_file` | Mount → `cat /mnt/test/file.txt` | Content matches server | RED→GREEN |
| `test_fuse_cp_file` | Mount → `cp local /mnt/test/new.txt` → verify | File exists on server | RED→GREEN |
| `test_fuse_mkdir` | Mount → `mkdir /mnt/test/newdir/` | Directory created | RED→GREEN |
| `test_fuse_rm_file` | Mount → `rm /mnt/test/file.txt` | File deleted | RED→GREEN |
| `test_fuse_mv_file` | Mount → `mv /mnt/test/a /mnt/test/b` | Renamed on server | RED→GREEN |
| `test_fuse_read_only` | Mount read-only → `touch /mnt/test/x` | EROFS error | RED→GREEN |

### 3.3 CLI Integration

| Test | Scenario | Expected | TDD Phase |
|------|----------|----------|-----------|
| `test_cli_mount_basic` | `rs-f4ss http://localhost:9000 /mnt/test` | Mount succeeds | RED→GREEN |
| `test_cli_mount_with_auth` | `rs-f4ss ... --user admin --pass secret` | Auth works | RED→GREEN |
| `test_cli_mount_read_only` | `rs-f4ss ... --read-only` | Writes rejected | RED→GREEN |
| `test_cli_unmount` | `rs-f4ss unmount /mnt/test` | Mount removed | RED→GREEN |
| `test_cli_status` | `rs-f4ss status` | Shows active mounts | RED→GREEN |
| `test_cli_invalid_url` | `rs-f4ss invalid-url /mnt/test` | Error message | RED→GREEN |
| `test_cli_missing_mountpoint` | `rs-f4ss http://...` | Error + usage | RED→GREEN |

---

## 4. E2E Tests

### 4.1 Full Stack (dufs server + rs-f4ss + filesystem ops)

```rust
#[tokio::test]
async fn e2e_full_workflow() {
    // 1. Start dufs server
    let server = start_dufs_server(&["--allow-all"]).await;

    // 2. Mount via CLI
    let mountpoint = tempdir().unwrap();
    let mount = start_dufs_mount(&server.url(), mountpoint.path()).await;

    // 3. Create file
    fs::write(mountpoint.join("test.txt"), "hello").unwrap();

    // 4. Verify on server (via WebDAV)
    let content = dav_get(&server.url(), "/test.txt").await;
    assert_eq!(content, "hello");

    // 5. Read back via mount
    let read_back = fs::read_to_string(mountpoint.join("test.txt")).unwrap();
    assert_eq!(read_back, "hello");

    // 6. Cleanup
    mount.unmount().await;
    server.stop().await;
}
```

### 4.2 E2E Test Matrix

| Test | Server Config | Mount Options | Operations |
|------|--------------|---------------|------------|
| `e2e_basic_rw` | `--allow-all` | default | ls, cat, cp, mv, rm |
| `e2e_auth_required` | `--auth admin:pass@/:rw` | `--user admin --pass pass` | ls, cat, cp |
| `e2e_read_only_server` | (no `--allow-all`) | `--read-only` | ls, cat (writes fail) |
| `e2e_large_file` | `--allow-all` | default | 100MB file cp |
| `e2e_many_files` | `--allow-all` | default | 1000 files in dir |
| `e2e_nested_dirs` | `--allow-all` | default | 10 levels deep |
| `e2e_unicode_names` | `--allow-all` | default | 文件名 with 中文 |
| `e2e_concurrent_read` | `--allow-all` | default | 10 parallel cat |
| `e2e_special_chars` | `--allow-all` | default | Filenames with spaces, `#`, `%`, `?` |
| `e2e_empty_file` | `--allow-all` | default | Create, read, write 0-byte files |
| `e2e_force_unmount` | `--allow-all` | default | `fusermount -uz` during active read → clean cleanup |
| `e2e_server_restart` | `--allow-all` | default | Restart dufs mid-session → reconnect behavior |

---

## 5. Performance Tests

### 5.1 Benchmarks

| Benchmark | Metric | Target | Method |
|-----------|--------|--------|--------|
| `bench_seq_read_100mb` | Throughput | >50 MB/s | `dd if=/mnt/test/big of=/dev/null` |
| `bench_seq_write_100mb` | Throughput | >30 MB/s | `dd if=/dev/zero of=/mnt/test/big bs=1M count=100` |
| `bench_random_read_4k` | IOPS | >1000 | `fio --name=randread --rw=randread --bs=4k` |
| `bench_list_10k_files` | Latency | <500ms | `time ls /mnt/test/bigdir/` |
| `bench_cache_hit_rate` | Hit % | >90% | Repeated `stat` on same files |
| `bench_mount_time` | Latency | <2s | Time from start to mount ready |

### 5.2 Profiling

```bash
# CPU profiling
cargo flamegraph --bin rs-f4ss-cli -- http://localhost:9000 /mnt/test

# Memory profiling
valgrind --tool=massif target/release/rs-f4ss-cli http://localhost:9000 /mnt/test

# I/O tracing
RUST_LOG=rs_f4ss=trace rs-f4ss http://localhost:9000 /mnt/test 2>&1 | grep -E "READ|WRITE"
```

---

## 6. Test Environment

### 6.1 Prerequisites

```bash
# Linux
sudo apt install fuse3 libfuse3-dev

# Start test dufs server
cargo run --bin dufs -- /tmp/test-files -p 9000 --allow-all

# Mount for manual testing
cargo run --bin rs-f4ss-cli -- http://localhost:9000 /mnt/test --foreground
```

### 6.2 CI Configuration

```yaml
# .github/workflows/test.yml
name: Tests
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Install FUSE
        run: sudo apt-get install -y fuse3 libfuse3-dev
      - name: Unit tests
        run: cargo test --lib
      - name: Integration tests
        run: cargo test --test integration
      - name: E2E tests
        run: cargo test --test e2e -- --test-threads=1
      - name: Clippy
        run: cargo clippy --all --all-targets -- -D warnings
      - name: Format check
        run: cargo fmt --all --check
```

### 6.3 Mock Backend for Testing

```rust
/// Mock backend that returns predefined responses
pub struct MockBackend {
    entries: HashMap<String, Vec<Entry>>,
    file_content: HashMap<String, Vec<u8>>,
}

impl MockBackend {
    pub fn new() -> Self { /* ... */ }
    pub fn add_file(&mut self, path: &str, content: &[u8]) { /* ... */ }
    pub fn add_dir(&mut self, path: &str) { /* ... */ }
}

#[async_trait]
impl StorageBackend for MockBackend {
    async fn stat(&self, path: &str) -> Result<Entry, BackendError> {
        self.entries.get(path)
            .and_then(|entries| entries.first())
            .cloned()
            .ok_or(BackendError::NotFound(path.to_string()))
    }
    // ... other methods
}
```

---

## 7. Acceptance Criteria

### 7.1 Phase 1 Definition of Done

- [x] All unit tests pass (`cargo test --all`) — 97 tests
- [x] E2E tests pass (4 phases: basic, readonly, auth, server-change) — 51 tests
- [x] Cross-platform: Linux (fuser) + Windows (WinFsp)
- [x] Clippy warnings = 0 (`cargo clippy --all-targets -- -W clippy::all`)
- [x] Code formatted (`cargo fmt --all -- --check`)
- [x] README with usage examples
- [x] DEV_GUIDE with setup instructions
- [x] SPEC + TASKS + TEST_PLAN + ADR documentation

### 7.2 Phase 2 Definition of Done

- [ ] Feature flags: `cargo build --no-default-features --features webdav` compiles
- [ ] S3 backend: 20+ new unit tests
- [ ] REST API: curl-based integration tests pass
- [ ] WebDAV Server: PROPFIND/GET/PUT/MKCOL/DELETE/MOVE all work
- [ ] All Phase 1 tests still pass (zero regression)
- [ ] Documentation updated (README, CLAUDE.md, SPEC, ADR)

### 7.2 Feature Acceptance Criteria

| Feature | Criteria |
|---------|----------|
| **Mount** | `rs-f4ss http://server:5000 /mnt/test` creates accessible mountpoint |
| **Read** | `cat /mnt/test/file.txt` returns correct content |
| **Write** | `cp file /mnt/test/new.txt` uploads to server |
| **List** | `ls /mnt/test/` shows server directory contents |
| **Delete** | `rm /mnt/test/file.txt` removes from server |
| **Rename** | `mv /mnt/test/a /mnt/test/b` renames on server |
| **Cache** | Second `stat` on same file is 10x+ faster than first |
| **Auth** | `--user admin --pass secret` authenticates correctly |
| **Read-only** | `--read-only` rejects writes with EROFS |
| **Unmount** | `fusermount -u /mnt/test` cleanly unmounts |

---

## 8. Test Data

### 8.1 Fixtures

```
tests/fixtures/
├── files/
│   ├── hello.txt          # "Hello, World!\n"
│   ├── binary.bin         # 1KB random bytes
│   ├── large.bin          # 100MB zeros (generated)
│   ├── unicode/文件名.txt  # Unicode filename
│   └── nested/
│       └── a/b/c/deep.txt # 10 levels deep
├── xml/
│   ├── propfind_single.xml    # Single file PROPFIND response
│   ├── propfind_dir.xml       # Directory PROPFIND response
│   └── propfind_empty.xml     # Empty directory PROPFIND response
└── scripts/
    └── start-dufs.sh      # Start test dufs server
```

### 8.2 PROPFIND XML Fixtures

```xml
<!-- propfind_single.xml -->
<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/hello.txt</D:href>
    <D:propstat>
      <D:prop>
        <D:getcontentlength>14</D:getcontentlength>
        <D:getlastmodified>Fri, 30 May 2026 10:00:00 GMT</D:getlastmodified>
        <D:resourcetype/>
      </D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>
```

---

*Test Plan maintained by viccom. Last updated 2026-05-30.*
