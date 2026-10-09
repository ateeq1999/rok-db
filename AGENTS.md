# Agent guide for rok-db

rok-db is a type-safe async ORM for PostgreSQL built on sqlx: `#[derive(Model)]`, typed column
constants, composable query builders and typed joins.

Start here:

- `llms.txt`: a dense digest of the public API and the conventions every API follows.
- `.agents/skills/`: task checklists. Read the matching skill before you start a task:

| Task | Skill |
|---|---|
| Run the checks CI runs | `.agents/skills/check/SKILL.md` |
| Write a commit message | `.agents/skills/commit/SKILL.md` |
| Open a pull request | `.agents/skills/pr/SKILL.md` |
| Propose a design (public API, dependency, MSRV, new database) | `.agents/skills/rfc/SKILL.md` |
| Change `#[derive(Model)]` or the `Model` trait | `.agents/skills/model/SKILL.md` |
| Change query building, SQL rendering or execution | `.agents/skills/query/SKILL.md` |
| Change `rok-db-gen` (SQL to Rust generation) or migrations | `.agents/skills/codegen/SKILL.md` |
| Review a change for code quality | `.agents/skills/quality/SKILL.md` |
| Cut a release | `.agents/skills/release/SKILL.md` |

Ground rules:

- Generated SQL quotes every identifier and binds every value as a parameter. Never format a
  value into SQL text.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` must be clean; every
  public item has a doc comment (`missing_docs`); `unsafe_code` is forbidden.
- Public futures are `Send`. Only the `rok-db` facade is public API; `rok-db-core` and
  `rok-db-macros` items it doesn't re-export, and anything `#[doc(hidden)]` or `__private`,
  may change.
- MSRV is `rust-version` in `Cargo.toml` (1.85); `.cargo/config.toml` makes the resolver pick
  MSRV-compatible dependencies.
- Database tests need `DATABASE_URL` (a user with `CREATEDB`); without it they skip silently,
  so "0.00s" test runs mean nothing was checked.
- Public API changes go through an RFC (`docs/rfcs/`) before implementation.
