---
name: quality
description: Review a rok-db change for code quality before committing or opening a pull request (SQL safety, correctness, API design, async, tests, docs, dependencies).
---

# Code quality review

Run the `check` skill first; this list covers what the tools don't catch. Read the whole diff
(`git diff origin/main...`), not only the last commit.

## SQL safety

- Every value is bound (`Sql::bind`, `?` in raw fragments); every identifier is quoted.
- No user input reaches `push`, `push_raw` or identifiers. Search the diff for `format!` near
  SQL.

## Correctness

- Implicit filters still apply: soft deletes, `default_scope`, tenant scope (fail-closed), and
  joined models' scopes in `ON`.
- Composite keys: code that used `PRIMARY_KEY` alone should usually use `PRIMARY_KEYS` /
  `key_values()`.
- Writes invalidate every table they touch; cached reads stamp every table they read.
- SQL is rendered inside futures, after task-locals (tenant, actor) are set.
- No `unwrap()` / `expect()` on database values or user input; errors are `rok_db::Error`
  variants with messages that say what to do.
- `LIMIT`/`OFFSET`/`DISTINCT ON` interact correctly with joins and ordering (add the root key
  as a tiebreaker where order must be total).

## API design

- Builder methods take `self` and return `Self`; new options don't break existing call
  sites. Names follow the existing API (`filter`, `order_by`, `all`, `first`, `one`).
- The compiler rejects misuse where it can (typed columns, `InScope` proofs); add a
  `compile_fail` doctest for each guarantee.
- Public surface stays small: internal helpers are `pub(crate)`; anything public but not
  supported is `#[doc(hidden)]`. New public API needs an accepted RFC.
- Breaking changes are listed in `CHANGELOG.md` with a migration path.

## Async and performance

- Public futures are `Send`; no blocking calls in async code.
- No per-row allocations that can be avoided in decoding; prefer streaming for large results.
- One round trip where possible (e.g. `paginate` fetches the count and the page in one
  statement).

## Tests and docs

- SQL-shape tests and Postgres tests that actually ran (`DATABASE_URL` set; not 0.00s).
- Tests check behaviour, not incidental SQL formatting, except in the SQL-shape tests.
- Every public item has rustdoc; examples compile; README and `CHANGELOG.md` updated;
  `llms.txt` updated for new concepts.

## Dependencies and compatibility

- New dependencies need an RFC, are optional behind a feature when possible, and pass
  `cargo deny check`.
- Builds on the MSRV (`cargo +1.85 check`); every feature builds on its own.

## Report

Group findings as: must fix (bugs, SQL safety, broken guarantees, failing checks), should fix
(missing tests or docs), optional (style). Fix the first two groups before opening the pull
request.
