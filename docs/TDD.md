# TDD in rs-f4ss

> Test-driven development is not a ceremony we perform; it is the way we ship.

This document explains **how** we use TDD on this project — the rhythm, the rules, the
tools, and what it looks like in real code. For *what* to test, see
[`TEST_PLAN.md`](TEST_PLAN.md). For *how to set up the dev environment*, see
[`DEV_GUIDE.md`](DEV_GUIDE.md).

---

## 1. The rhythm: RED → GREEN → REFACTOR → COMMIT

Every change follows the same loop. No exceptions.

```
       ┌──────────────────────────────────────────────────┐
       │                                                  │
       ▼                                                  │
   ┌───────┐    ┌───────┐    ┌──────────┐    ┌──────────┐ │
   │  RED  │───▶│ GREEN │───▶│ REFACTOR │───▶│  COMMIT  │─┘
   └───────┘    └───────┘    └──────────┘    └──────────┘
   Write a     Write the     Clean up.       Small,
   failing     minimum       Tests still    focused,
   test.       code to       green.         conventional
               pass.                        commit.
```

### What "RED" means here

A red test is not "the test compiles but the assertion is wrong." A red test is:

- A new `#[test]` or `#[tokio::test]` block in a `#[cfg(test)]` module
- The test calls code that **does not exist yet**, or exists but returns a hard-coded
  wrong value
- `cargo test` reports a clear, narrow failure pointing at the missing behavior
- The failure message reads like a requirement: *"when X happens, the system should Y"*

If the red test compiles cleanly and fails because the production code is missing, you
are doing TDD correctly. If you find yourself writing the production code first and
back-fitting the test, **stop and delete the production code** — it's a smell.

### What "GREEN" means here

The smallest possible change to production code that makes the red test pass. Not
"the right design." Not "clean code." Just **green**. Design happens in REFACTOR.

> _"Make it work, make it right, make it fast"_ — Kent Beck, _Test-Driven Development
> by Example_, 2002.

### What "REFACTOR" means here

- Rename for clarity
- Extract duplication
- Tighten types
- Add doc comments

You may refactor only while all tests are green. If a refactor breaks a test, the test
was load-bearing — that test was telling you something about the design.

### What "COMMIT" means here

A small, focused commit. One logical change. Conventional Commits format:

```
<type>(<scope>): <subject>
```

| Type | Use for |
|------|---------|
| `feat` | New user-visible behavior |
| `fix` | Bug fix |
| `test` | Adding/updating tests only (no production code change) |
| `refactor` | Neither user-visible behavior change nor bug fix |
| `docs` | Documentation only |
| `chore` | Build, CI, dependencies, formatting |

---

## 2. The pyramid

```
                          ╱╲
                         ╱ E ╲         few, slow, high confidence
                        ╱─────╲        — full system, real servers
                       ╱       ╲
                      ╱ Integ.  ╲     moderate, medium speed
                     ╱───────────╲    — module interaction, MockBackend
                    ╱             ╲
                   ╱    Unit       ╲  many, fast, low cost
                  ╱─────────────────╲ — single function/struct, < 1 ms
```

### Targets

| Layer | Coverage target | What it covers |
|-------|-----------------|----------------|
| **Unit** | 80%+ line coverage | Pure logic: parsers, encoders, error mapping, state machines |
| **Integration** | 60%+ of public API surface | Backend + cache, FUSE/WinFsp callback → backend round-trip |
| **E2E** | All golden-path scenarios | Real `dufs` server, real FUSE, real shell commands |

E2E tests are intentionally **few** — they are slow, flaky on CI, and don't pinpoint
failures. A failure in unit tests is a 5-minute fix; a failure in E2E is a 2-hour
investigation. Build the cheap safety net first.

### What goes where

| Behavior | Layer | Why |
|----------|-------|-----|
| XML PROPFIND response → `Entry` struct | Unit | Pure function, no I/O |
| `Entry { is_dir: true }` → `FileAttr` | Unit | Pure mapping |
| `HttpClient::build_url` with auth | Unit | String building |
| `CacheLayer::get` after `set` | Unit | State machine |
| Cache invalidation on write | Integration | Requires mock backend |
| `mkdir` via FUSE reaches backend | Integration (Linux) | Requires `fuser::TestSession` or `/dev/fuse` |
| `cat /mnt/test/file.txt` works on real dufs | E2E | Real server, real FUSE, real shell |

---

## 3. Conventions

### 3.1 Test naming

```
test_<unit>_<scenario>_<expected_outcome>
```

Examples from the codebase:

```rust
test_read_from_dirty_uses_unflushed_buffer
test_write_at_invalid_handle_reports_specific_error
test_failed_first_write_rolls_back_placeholder
test_password_none_not_in_json
test_backend_new_readonly
test_io_error_mapping_readonly
test_render_viewer_read_only_flag
test_map_backend_error_readonly
```

Read the name aloud. If it doesn't form a complete sentence
(_"the function, when given X, should do Y"_), rename it.

### 3.2 File placement

- **Unit tests** live in the same file as the production code, in a `#[cfg(test)] mod tests` block.
  This keeps the test next to the thing it tests and makes the relationship obvious.

  ```rust
  // crates/rs-f4ss-core/src/handle.rs

  pub struct HandleTable { /* ... */ }

  #[cfg(test)]
  mod tests {
      use super::*;

      #[test]
      fn test_write_at_invalid_handle_reports_specific_error() {
          let table = HandleTable::new();
          let result = table.write_at(/* invalid */ 9999, 0, b"x");
          assert!(matches!(result, Err(HandleError::InvalidHandle)));
      }
  }
  ```

- **Integration tests** live in `tests/` at the workspace root, one `.rs` file per
  scenario (`tests/integration_backend.rs`, `tests/integration_cache.rs`, ...).
  They use the public API only — no `#[cfg(test)]` access.

- **E2E tests** are shell scripts (`tests/e2e.sh`, `tests/e2e-api.sh`,
  `tests/e2e-share.sh`) and PowerShell (`tests/e2e.ps1`) for cross-platform coverage.
  They start a real `dufs` server as a child process, mount via the CLI, run shell
  commands, assert on the results.

### 3.3 Mocks vs. fakes

We have **one** in-tree test double: `MockBackend` in `mount.rs`. It implements
`StorageBackend` with a `HashMap<String, Vec<Entry>>` for directory listings and
predefined file contents. Use it for:

- MountEngine unit/integration tests that need a backend
- FUSE callback tests that don't want network

Do **not** introduce a general-purpose mocking library (`mockall`, `mockito`) for one-off
mocks. The cost (build time, complexity, abstraction tax) outweighs the benefit at our
size. If you need a second mock, copy the pattern from `MockBackend`.

### 3.4 Async tests

```rust
#[tokio::test]
async fn test_async_behavior() {
    let backend = MockBackend::new();
    let result = backend.stat("/x").await;
    assert!(result.is_ok());
}
```

Use the multi-threaded runtime (default for `#[tokio::test]`) unless you specifically
need to test single-threaded behavior, in which case:

```rust
#[tokio::test(flavor = "current_thread")]
async fn test_no_spawn() { /* ... */ }
```

### 3.5 Assertions

Prefer specific assertions over generic `assert!`:

```rust
// ✗ vague
assert!(result.is_ok());

// ✓ specific
assert_eq!(result.unwrap(), expected_entry);
assert!(matches!(result, Err(BackendError::NotFound(_))));
```

For complex expectations, factor the assertion into a helper function and call it from
multiple tests. The helper's name becomes a sentence:

```rust
fn assert_round_trips(original: &Entry) {
    let attr = original.to_file_attr();
    assert_eq!(attr.size, original.size);
    assert_eq!(attr.mtime, original.mtime);
    assert_eq!(attr.kind, original.to_file_type());
}
```

### 3.6 What NOT to test

- **Trivial getters/setters** — they have no behavior. A `set_x`/`get_x` pair is
  tested by any other test that happens to use both.
- **The standard library** — don't test that `Vec::push` appends.
- **Generated code** — out of scope.
- **Pure UI rendering** (HTML strings) — the test only proves the strings are equal to
  the strings. A snapshot test on the *shape* (does it contain the expected DOM nodes?)
  is more valuable than a literal string match. We use `insta` snapshots in
  `server/autoindex.rs` and `server/viewer.rs`.

---

## 4. Worked example: write buffering in `HandleTable`

This is a real TDD sequence from the project — captured in commit
`a368c35` and earlier.

### Step 1 — RED

A bug was reported: if a write to a freshly-created file fails on the first `PUT`, the
file is left as a zero-byte placeholder on the server. We need to roll it back.

Write the test first:

```rust
#[test]
fn test_failed_first_write_rolls_back_placeholder() {
    let ctx = FileContext::new_test("/placeholder.bin", false, true);
    ctx.record_write_failure();
    assert_eq!(ctx.close_action(), CloseAction::RollbackPlaceholder);
}
```

Run it. It fails — `CloseAction::RollbackPlaceholder` doesn't exist yet, and
`record_write_failure` is a stub. **Good, that's the red.**

### Step 2 — GREEN

Add the minimum to make it pass:

```rust
pub enum CloseAction {
    Commit,
    RollbackPlaceholder,  // ← new
}

impl FileContext {
    pub fn record_write_failure(&mut self) {
        self.first_write_failed = true;
    }

    pub fn close_action(&self) -> CloseAction {
        if self.first_write_failed && self.is_new_file {
            CloseAction::RollbackPlaceholder
        } else {
            CloseAction::Commit
        }
    }
}
```

Run the test. It passes. **Green.**

### Step 3 — REFACTOR

Now the test gives us the safety net to clean up:

- Extract `is_new_file && first_write_failed` into `should_rollback_placeholder()`.
- Add doc comments.
- Add two more tests for the other branches of `close_action`:
  - `test_successful_first_write_commits` (the happy path)
  - `test_failed_write_on_existing_file_still_commits` (overwrite of existing file)

Run all three. All green. **Refactor complete.**

### Step 4 — COMMIT

```
fix(mount): rollback placeholder on failed first write

When a file was newly created and the first PUT failed, a 0-byte
placeholder was left on the server. Track first-write state in
FileContext and emit a DELETE on close in that case.

Tests:
  test_failed_first_write_rolls_back_placeholder (new)
  test_successful_first_write_commits (new)
  test_failed_write_on_existing_file_still_commits (new)
```

---

## 5. Tooling

### 5.1 Built-in

- **`cargo test`** — the only test runner we use. `#[test]` and `#[tokio::test]` are
  the only test attributes.
- **`cargo fmt --check`** — formatting is a CI gate, not a style preference.
- **`cargo clippy --all-targets --all-features -- -W clippy::all`** — clippy is a CI
  gate. Zero warnings.

### 5.2 Dev-dependencies (selected)

```toml
[dev-dependencies]
tokio = { workspace = true, features = ["test-util", "macros"] }
tower = "0.5"           # for testing axum routers
tempfile = "3"          # for tempdir in integration tests
```

If you need a new dev-dependency, justify it in the PR. We default to as few
dependencies as possible.

### 5.3 What we deliberately don't use

- `mockall` — our needs are met by hand-written `MockBackend`. The macro cost and
  compile time aren't worth it.
- `rstest` / `test-case` — parameterization is useful but adds a dependency. For
  one-off parameterized tests, a `for` loop over a slice of inputs is fine.
- `criterion` — benchmarking is not on the critical path for v0.x. `cargo bench` +
  `std::time::Instant` is enough when we need it.

---

## 6. Coverage

We do **not** run `cargo tarpaulin` in CI (it doesn't support our async runtime
configurations reliably). Instead we rely on:

1. **The TEST_PLAN matrix** — every line in the plan has a corresponding test. The
   plan is the contract.
2. **The CI gate** — `cargo test --all-features` must pass on every PR.
3. **The pre-merge review** — see [`CONTRIBUTING.md`](../CONTRIBUTING.md) §4.

If you add a new code path, add a test for it in the same commit. If you remove a
behavior, remove the test in the same commit. The diff is the documentation.

---

## 7. CI integration

Every push and PR runs the full TDD cycle on Linux + Windows:

```yaml
# .github/workflows/ci.yml
- cargo fmt --all -- --check
- cargo clippy --all-targets --all-features -- -W clippy::all
- cargo test --all-features
- cargo test --no-default-features --features webdav
- cargo test --no-default-features --features http
- cargo test --no-default-features --features serve
- cargo build --release --all-features
```

A red CI is a red build. Do not merge around it. Either fix the test or fix the code —
never `#[ignore]`, never `#[cfg(skip)]`, never `git commit --no-verify` to bypass.

---

## 8. Anti-patterns

| Anti-pattern | Why it's wrong | What to do instead |
|--------------|----------------|---------------------|
| Writing the production code first, then tests "to match" | The test never has a chance to fail. You can't trust a test that was written to confirm what you already wrote. | Delete the production code. Start with the test. |
| `assert!(x.is_ok())` because the actual value doesn't matter | You're testing the type, not the behavior. | Compare the value: `assert_eq!(x.unwrap(), expected)`. |
| One giant `#[test] fn test_everything()` | Failure messages are useless. A 5-line test that breaks is a 5-minute fix. A 500-line test that breaks is archaeology. | One assertion per test (loosely). |
| `#[ignore]` on a failing test to "come back to it" | It rots. The test stops being run, the behavior drifts, and the original intent is lost. | Fix the test now. If the test is genuinely not ready, mark it `#[ignore = "reason"]` and open an issue. |
| Mocking the filesystem in E2E | You are testing your mock, not your code. | Use real `/tmp`, real `dufs`, real FUSE. |
| Skipping the commit because "the tests are passing" | You lose the audit trail. TDD's value is in the loop, not the destination. | Commit after every green. |
| Adding a test for a bug fix *after* the fix | The test is a description of the fix, not a description of the requirement. The fix could be wrong. | Add the test first. It should fail. Then make it pass. |

---

## 9. References

- Kent Beck, _Test-Driven Development by Example_ (2002)
- Martin Fowler, _Refactoring_ (2nd ed., 2018) — for the REFACTOR step
- [`docs/TEST_PLAN.md`](TEST_PLAN.md) — what to test
- [`CONTRIBUTING.md`](../CONTRIBUTING.md) §2.1 — the workflow summary
- [Rust API Guidelines: Testing](https://rust-lang.github.io/api-guidelines/)

---

*Last updated: 2026-06-08. Maintained by the rs-f4ss contributors.*
