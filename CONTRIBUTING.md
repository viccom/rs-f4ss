# Contributing to rs-f4ss

Thank you for your interest in contributing! This guide explains how to participate.

---

## 1. Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/rs-f4ss.git`
3. Create a feature branch: `git checkout -b feature/my-feature`
4. Follow the [Development Guide](docs/DEV_GUIDE.md) to set up your environment

---

## 2. Development Workflow

### 2.1 TDD (Test-Driven Development)

We follow TDD strictly:

```
RED → GREEN → REFACTOR → COMMIT
```

1. **RED**: Write a failing test that describes the behavior you want
2. **GREEN**: Write the minimum code to make the test pass
3. **REFACTOR**: Clean up the code while keeping tests green
4. **COMMIT**: Make a small, focused commit

### 2.2 Before Committing

```bash
# All tests must pass
cargo test --all

# No clippy warnings
cargo clippy --all --all-targets -- -D warnings

# Code must be formatted
cargo fmt --all --check

# Coverage should not decrease
cargo tarpaulin --all
```

### 2.3 Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer]
```

Types:
- `feat`: New feature
- `fix`: Bug fix
- `test`: Adding or updating tests
- `refactor`: Code change that neither fixes a bug nor adds a feature
- `docs`: Documentation changes
- `chore`: Maintenance tasks

Examples:
```
feat(backend): add SFTP storage backend
test(cache): add TTL expiration tests
fix(webdav): handle empty PROPFIND response
docs(readme): update installation instructions
```

---

## 3. Code Standards

### 3.1 Style

- Follow [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- Use `rustfmt` with default settings
- Use `clippy` with `-D warnings` (warnings are errors)
- Maximum line length: 100 characters
- Prefer `snake_case` for functions/variables, `PascalCase` for types

### 3.2 Documentation

- All public APIs must have doc comments (`///`)
- Include examples in doc comments where helpful
- Update `SPEC.md` when adding/changing features
- Update `TEST_PLAN.md` when adding test cases

### 3.3 Error Handling

- Use `thiserror` for error types
- Use `anyhow` for application-level errors
- Never use `unwrap()` in library code (use `?` or `expect()`)
- Always provide meaningful error messages

### 3.4 Testing

- Unit tests in the same file as the code (inline `#[cfg(test)]` modules)
- Integration tests in `tests/` directory
- E2E tests use real dufs server (start as child process)
- Use `MockBackend` for testing without network

---

## 4. Pull Request Process

### 4.1 PR Checklist

Before submitting a PR, ensure:

- [ ] All tests pass (`cargo test --all`)
- [ ] No clippy warnings (`cargo clippy -- -D warnings`)
- [ ] Code formatted (`cargo fmt --check`)
- [ ] Documentation updated (if applicable)
- [ ] Test coverage maintained or improved
- [ ] Commit messages follow Conventional Commits
- [ ] PR description explains what and why

### 4.2 PR Template

```markdown
## Description
Brief description of changes.

## Motivation
Why is this change needed?

## Type of Change
- [ ] New feature
- [ ] Bug fix
- [ ] Refactoring
- [ ] Documentation
- [ ] Test

## Testing
How was this tested?

## Checklist
- [ ] Tests pass
- [ ] Clippy clean
- [ ] Formatted
- [ ] Docs updated
```

### 4.3 Review Process

1. Submit PR with clear description
2. Automated checks run (CI)
3. Maintainer reviews code
4. Address feedback
5. Maintainer approves and merges

---

## 5. Issue Guidelines

### 5.1 Bug Reports

```markdown
## Bug Description
What happened?

## Expected Behavior
What should have happened?

## Steps to Reproduce
1. Start dufs server with ...
2. Mount with command ...
3. Run operation ...
4. See error ...

## Environment
- OS: Linux/macOS/Windows
- Rust version: 1.XX
- dufs version: X.XX
- FUSE driver: libfuse3/macFUSE/WinFsp

## Logs
```
RUST_LOG=rs_f4ss=debug rs-f4ss ... 2>&1
```
```

### 5.2 Feature Requests

```markdown
## Feature Description
What feature would you like?

## Use Case
Why do you need this?

## Proposed Solution
How should it work?

## Alternatives Considered
What other approaches did you consider?
```

---

## 6. Adding a New Backend

See [DEV_GUIDE.md Section 5](docs/DEV_GUIDE.md#5-adding-a-new-backend) for detailed steps.

Summary:
1. Write tests for the new backend
2. Implement `StorageBackend` trait
3. Register in `backend/mod.rs`
4. Add URL scheme to CLI `resolve_backend`
5. Update `SPEC.md` and `README.md`
6. Submit PR

---

## 7. License

By contributing, you agree that your contributions will be licensed under the MIT License.

---

## 8. Questions?

Open an issue or start a discussion. We're happy to help!

---

*Thank you for contributing to rs-f4ss!*
