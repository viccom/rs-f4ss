## Summary

<!-- One paragraph: what does this PR do and why? Link the issue it closes with `Closes #123`. -->

Closes #

## Type of change

- [ ] Bug fix (non-breaking change that fixes an issue)
- [ ] New feature (non-breaking change that adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to change)
- [ ] Documentation only
- [ ] Refactor (no functional change)
- [ ] Test addition / improvement
- [ ] CI / build / dependency update

## How was this tested?

<!-- Tick all that apply. The CI will run the rest. -->

- [ ] Unit tests added/updated (`cargo test --all-features`)
- [ ] Integration tests added/updated
- [ ] E2E tested locally (`bash tests/e2e.sh`)
- [ ] Manual smoke test (describe below)

```
$ cargo test ...
```

## Checklist

### Code

- [ ] `cargo fmt --all -- --check` is clean
- [ ] `cargo clippy --all-targets --all-features -- -W clippy::all` is clean
- [ ] No new warnings introduced
- [ ] No `unwrap()` / `expect()` added in library code (use `?` or typed errors)
- [ ] No `#[ignore]` added (fix the test or open a follow-up issue)

### TDD

- [ ] If this is a behavioral change, the test came **first** (see [`docs/TDD.md`](../blob/master/docs/TDD.md))
- [ ] The test fails on `master` without the production change
- [ ] The test passes with the production change

### Docs

- [ ] `README.md` updated (if user-visible)
- [ ] `CHANGELOG.md` updated (Unreleased section)
- [ ] `docs/SPEC*.md` updated (if architecture changed)
- [ ] `docs/ADR.md` updated (if a design decision was made — add a new ADR)
- [ ] `docs/TEST_PLAN.md` updated (if new test categories added)

### Commits

- [ ] Commits follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] Commits are small, focused, and have descriptive messages
- [ ] No `Co-Authored-By: ...` trailer added (per repo policy)

## Screenshots / output

<!-- If relevant, show before/after, screenshots, or example CLI output. -->

```bash
$ rs-f4ss ...
```

## Risk and rollout

<!-- What could break? How do we roll back? Does it need a release note? -->

- Backward compatible? <!-- yes / no -->
- Needs migration? <!-- yes / no -->
- Feature flag? <!-- yes / no, or N/A -->
- Plan B if this fails in production: <!-- revert commit, hotfix, ... -->

## Reviewer notes

<!-- Anything the reviewer should pay extra attention to. -->
