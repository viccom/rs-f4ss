# Code Review Fix — Task Checklist

## Phase 1: Low-risk cleanup
- [x] Task 7: Remove unused serde_json dependency
- [x] Task 3: Deduplicate error mapping (errno_from_backend → map_backend_error)
- [x] Checkpoint: `cargo check` + `cargo test --all` ✅

## Phase 2: P0 critical fixes
- [x] Task 1: read() Range-based chunked reading with fallback
- [x] Task 2: readdirplus eliminate N+1 stat calls
- [x] Checkpoint: Full E2E test (`tests/e2e.sh`) — 36/36 ✅

## Phase 3: P1 improvements
- [x] Task 5: Add --pass-file / DUFS_MOUNT_PASSWORD env var
- [x] Task 4: Write mount.rs unit tests (25 new tests)
- [x] Checkpoint: `cargo test --all` — 98 passed ✅

## Phase 4: Polish
- [x] Task 6: cargo fmt + clippy cleanup (0 code warnings)
- [x] Final verification: all checks pass ✅

## Review

| Metric | Before | After |
|--------|--------|-------|
| Unit tests | 73 | 98 (+25) |
| E2E tests | 36/36 | 36/36 |
| Clippy warnings | ~30 | 0 |
| Fmt issues | Yes | Clean |
| read() perf | Full download × N | Range × 1 |
| readdirplus HTTP calls | N+1 | 2 |
| Password security | cmdline only | cmdline / file / env |
