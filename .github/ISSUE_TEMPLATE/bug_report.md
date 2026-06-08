---
name: Bug report
about: Report something that's broken
title: "[bug] "
labels: ["bug"]
assignees: []
---

## What happened?

<!-- A clear, one-sentence description of the bug. -->

## Expected behavior

<!-- What did you expect to happen? -->

## Steps to reproduce

<!-- Minimal steps — the shortest path from a clean checkout to seeing the bug. -->

1.
2.
3.

## Environment

- OS / distribution: <!-- e.g. Ubuntu 24.04, Windows 11 23H2 -->
- Kernel (Linux) or build (Windows):
- Rust version (`rustc --version`):
- rs-f4ss version (`rs-f4ss --version` or commit hash):
- FUSE driver: <!-- libfuse3 / WinFsp / macFUSE / version -->
- Backend server: <!-- dufs 0.43.0 / nginx 1.27 / Nextcloud 29 / ... -->
- Install method: <!-- prebuilt binary / `cargo install` / `cargo build` -->

## Configuration

<!-- The exact command line or mount config you used. Mask any passwords. -->

```bash
rs-f4ss http://... /mnt/test --user x --pass *** --read-only
```

## Logs

<!-- Run with `RUST_LOG=rs_f4ss=debug` (or =trace) and paste the relevant output. -->

```
RUST_LOG=rs_f4ss=debug rs-f4ss ...
```

## Impact

<!-- How severe is this? Does it block your work? Is there a workaround? -->

- [ ] Blocks my work
- [ ] Workaround available (describe below)
- [ ] Cosmetic / minor

## Notes

<!-- Anything else that might help — related issues, screenshots, your analysis. -->
