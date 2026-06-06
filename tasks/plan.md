# Code Review Fix Plan

**Date**: 2026-05-31
**Scope**: 7 issues from Five-Axis code review
**Approach**: Vertical slices, each task is a complete fix → test → verify

---

## Dependency Graph

```
Task 7 (remove serde_json)     ← independent
Task 3 (error dedup)           ← independent, touches error.rs + mount.rs
Task 1 (read Range)            ← independent, touches webdav.rs
Task 2 (readdirplus fix)       ← depends on understanding Entry→FileAttr
Task 5 (pass-file/env)         ← independent, touches main.rs only
Task 4 (mount unit tests)      ← MUST come after Task 2 + Task 3 (mount.rs changes)
Task 6 (fmt + clippy)          ← MUST come last (reformats everything)
```

## Execution Order

| Phase | Task | Priority | Files | Risk |
|-------|------|----------|-------|------|
| 1 | #7 Remove unused serde_json | P2 | Cargo.toml | Zero |
| 1 | #3 Deduplicate error mapping | P1 | error.rs, mount.rs | Low |
| 2 | #1 read() Range-based chunking | P0 | webdav.rs | Medium |
| 2 | #2 readdirplus from list() Entry | P0 | mount.rs | Medium |
| 3 | #5 --pass-file / env var | P1 | main.rs | Low |
| 3 | #4 mount.rs unit tests | P1 | mount.rs | Low |
| 4 | #6 cargo fmt + clippy | P2 | All files | Zero |

## Checkpoint Strategy

- **After Phase 1**: `cargo check` + `cargo test --all`
- **After Phase 2**: Full E2E test (`tests/e2e.sh`) — this is the critical gate
- **After Phase 3**: `cargo test --all` + manual CLI verification
- **After Phase 4**: `cargo fmt --check` + `cargo clippy` clean

---

## Task Details

### Task 7: Remove unused serde_json dependency
- **Files**: `Cargo.toml` (workspace)
- **Action**: Remove `serde_json = "1"` from `[workspace.dependencies]`
- **Verify**: `cargo check` passes
- **Lines changed**: ~1

### Task 3: Deduplicate error mapping functions
- **Files**: `error.rs`, `mount.rs`
- **Action**:
  1. Keep `map_backend_error()` in `error.rs` (public, used by tests)
  2. Replace private `errno_from_backend()` in `mount.rs` with `map_backend_error()`
  3. `error.rs` already imports `fuser` — verify it compiles
- **Verify**: `cargo test --all` passes (tests use both functions)
- **Lines changed**: ~15

### Task 1: read() Range-based chunked reading
- **Files**: `webdav.rs`
- **Current behavior**: Downloads entire file on every read() call, then slices
- **Target behavior**: Use HTTP Range header; fall back to full download on 416
- **Action**:
  1. Add `Range: bytes=offset-offset+size` header to GET request
  2. If server returns 416 (Range Not Satisfiable), fall back to full download + slice
  3. If server returns 206 (Partial Content), use the range response directly
  4. If server returns 200 (full content), slice as before (server ignored Range)
- **Risk**: Different WebDAV servers handle Range differently; fallback ensures compatibility
- **Verify**: E2E test passes (dufs supports Range headers)
- **Lines changed**: ~30

### Task 2: readdirplus from list() Entry (no extra stat)
- **Files**: `mount.rs`
- **Current behavior**: `readdirplus` calls `backend.stat()` per entry → N+1 PROPFIND
- **Target behavior**: Use `list()` returned Entry data to construct FileAttr directly
- **Action**:
  1. `list()` already returns `Vec<Entry>` with size/mtime/is_dir
  2. Convert each Entry to FileAttr using existing `entry_to_file_attr()`
  3. For "." and ".." entries, use the parent dir's own Entry data
  4. Eliminate all `backend.stat()` calls inside readdirplus
- **Verify**: E2E test passes (ls -la still works)
- **Lines changed**: ~30

### Task 5: --pass-file / env var for password
- **Files**: `main.rs`
- **Action**:
  1. Add `--pass-file` CLI arg: reads password from file
  2. Support `DUFS_MOUNT_PASSWORD` env var as fallback
  3. Priority: --pass > --pass-file > env var
  4. Update CLI help text
- **Verify**: `cargo test --all` passes (add tests for new args)
- **Lines changed**: ~25

### Task 4: mount.rs unit tests
- **Files**: `mount.rs`
- **Current state**: Test module is empty placeholder
- **Action**: Write tests for FUSE callback logic using MockBackend:
  1. Test `entry_to_file_attr()` helper (unit)
  2. Test `errno_from_backend()` error mapping (unit)
  3. Test FuseAdapter inherent methods (read/write/flush/mkdir/rmdir/unlink/rename)
- **Note**: Full FUSE trait tests require kernel interaction — test inherent methods only
- **Verify**: `cargo test --all` passes with new tests
- **Lines changed**: ~80

### Task 6: cargo fmt + clippy cleanup
- **Files**: All .rs files
- **Action**:
  1. Run `cargo fmt` to fix all formatting
  2. Fix clippy warnings:
     - `let _ = reply.add(...)` for must_use
     - `as usize` → safe cast with `.try_into()` for u64→usize
     - Remove unused imports (super::*, Mutex in test module)
     - Remove `async` from fns with no await
     - Inline format variables
     - Fix variable name similarity
  3. Verify `cargo clippy --all-targets` has zero warnings
- **Verify**: `cargo fmt --check` clean + `cargo clippy` clean
- **Lines changed**: ~50

---

## Risk Assessment

| Risk | Mitigation |
|------|-----------|
| Range headers break dufs | Fallback to full download on 416/200 |
| readdirplus behavior change | E2E test covers ls -la |
| clippy fixes introduce bugs | Run tests after each fix group |

## Final Verification

After all tasks complete:
1. `cargo check` — zero errors
2. `cargo test --all` — all pass
3. `cargo fmt --check` — clean
4. `cargo clippy --all-targets` — zero warnings
5. `tests/e2e.sh` — 36/36 pass
