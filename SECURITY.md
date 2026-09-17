# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.3.x   | ✅ |
| < 0.3.0 | ❌ |

## Reporting a Vulnerability

Please report security issues by opening a
[private security advisory](https://github.com/viccom/rs-f4ss/security/advisories/new)
or contacting the maintainer directly.

Please include:

- A description of the issue and its impact
- Steps to reproduce (a minimal PoC is appreciated)
- Affected versions / commit range

You can expect an initial response within a week. Please do not open a public
issue for anything you believe is exploitable.

## Known Security-Relevant Behaviours

- **Mount passwords in `config.json` are stored in plaintext.** File
  permissions are tightened to 0600 on Unix only; Windows applies no ACL
  hardening. Protect the configuration directory accordingly.
- **`share serve` binds to `127.0.0.1` by default.** Binding a non-loopback
  address without `--user`/`--pass` prints a warning and exposes the shared
  directory to anyone who can reach it.
- **`rs-f4ss serve` refuses to start with default credentials
  (`admin`/`admin`) on a non-loopback address.**
- Self-updates are **SHA-256-only over HTTPS**; there is no signature chain.
