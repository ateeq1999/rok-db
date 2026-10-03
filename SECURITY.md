# Security Policy

## Supported versions

rok-db is pre-1.0. Security fixes are released for the latest minor version
only.

| Version | Supported |
|---------|-----------|
| 0.1.x   | ✅        |
| < 0.1   | ❌        |

## Reporting a vulnerability

**Please do not report security vulnerabilities in public issues, discussions
or pull requests.**

Report them privately through GitHub:
**Security → Advisories → [Report a vulnerability](https://github.com/ateeq1999/rok-db/security/advisories/new)**.

Please include:

- the affected version(s) and feature flags,
- a description of the issue and its impact (for example SQL injection, data
  leakage between transactions, or credential exposure in logs or errors),
- a minimal reproduction (Rust code and schema) if possible.

## What to expect

- Acknowledgement within **3 business days**.
- An initial assessment and severity rating within **10 business days**.
- A fix, a coordinated release date and a GitHub Security Advisory (with a
  CVE when appropriate) for confirmed issues. We aim to release fixes for
  critical issues within 30 days.
- Credit in the advisory, unless you prefer to remain anonymous.

## Scope

In scope: SQL generation and parameter binding, identifier quoting, the
derive macro's generated code, connection and transaction handling, and
anything that could leak data or credentials.

Out of scope: vulnerabilities in PostgreSQL or sqlx themselves (please report
those upstream), and SQL written by users through `raw()`, `Expr::raw` or
`set_raw`, which is executed as written by design.
