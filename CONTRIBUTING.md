# Contributing to rok-db

Thanks for helping. This guide explains how changes get proposed, reviewed,
merged and released. Everyone taking part agrees to follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

- [Ways to contribute](#ways-to-contribute)
- [Development setup](#development-setup)
- [Change protocol](#change-protocol)
- [Commit messages](#commit-messages)
- [Pull request checklist](#pull-request-checklist)
- [Coding standards](#coding-standards)
- [Compatibility policy](#compatibility-policy)
- [Release process](#release-process)
- [Coding agents](#coding-agents)

## Ways to contribute

| You want to…                     | Do this                                                                 |
|----------------------------------|-------------------------------------------------------------------------|
| Report a bug                     | Open a [bug report](../../issues/new?template=bug_report.yml)           |
| Report a security issue          | Follow [SECURITY.md](SECURITY.md); **never** open a public issue        |
| Ask a question                   | Open a [Discussion](../../discussions)                                  |
| Suggest a small improvement      | Open a [feature request](../../issues/new?template=feature_request.yml) |
| Propose a large or public-API change | Write an [RFC](docs/rfcs/README.md)                                 |
| Fix a bug or docs                | Send a pull request directly                                            |

Issues labelled `good first issue` and `help wanted` are good starting points.
Comment on an issue before starting work so effort isn't duplicated.

## Development setup

Requirements: Rust (stable, plus the MSRV toolchain if you touch
dependencies) and PostgreSQL 14+ for the end-to-end tests.

```sh
git clone https://github.com/ateeq1999/rok-db && cd rok-db

# Unit and SQL-generation tests (no database needed)
cargo test --workspace --all-features

# End-to-end tests against PostgreSQL
docker run -d --name rok-db-pg -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=rok_db_test -p 5432:5432 postgres:16
export DATABASE_URL=postgres://postgres:postgres@localhost/rok_db_test
cargo test --workspace --all-features
```

The database tests use per-connection `TEMP` tables or, with the `testing`
feature, a temporary database per test (`#[rok_db::test]`), so they never
touch existing data and can run in parallel. The `DATABASE_URL` user needs
the `CREATEDB` privilege.

Before pushing, run the same checks as CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo +1.85 check --workspace --all-targets --all-features   # MSRV
cargo deny check                                             # licenses and advisories
```

### Repository layout

| Path                    | Contents                                                     |
|-------------------------|--------------------------------------------------------------|
| `crates/rok-db`         | Public facade crate: re-exports, prelude, docs, tests, examples |
| `crates/rok-db-core`    | Runtime: `Db`, `Model`, query builders, `Value`, errors       |
| `crates/rok-db-macros`  | `#[derive(Model)]`                                           |
| `crates/rok-db-codegen` | `rok-db-gen`: a crate from `.sql` files (see `docs/v4.md`)   |
| `examples/sqlgen`       | example project with its generated crate committed           |
| `docs/rfcs`             | Design proposals                                             |

## Change protocol

Every change follows the same path; the size of the change decides where it
starts.

1. **Discuss** — bugs and small features start as an issue. Changes to the
   public API, new crates or features, new dependencies, MSRV bumps and
   anything breaking start as an [RFC](docs/rfcs/README.md).
2. **Approve** — a maintainer labels the issue `accepted` (or merges the RFC).
   Pull requests for unapproved features may be closed, so wait for this step
   for anything non-trivial.
3. **Implement** — branch from `main`, one logical change per pull request.
   Fill in the pull request template and link the issue (`Closes #123`).
4. **Review** — at least one maintainer approval and green CI are required.
   Address every comment with a change or a reply; reviewers resolve threads.
5. **Merge** — maintainers squash-merge, so the pull request title becomes the
   commit message and must follow [Conventional Commits](#commit-messages).
6. **Release** — changes ship with the next release (see below).

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/):

```
<type>(<optional scope>): <summary in the imperative, lower case>

<optional body: what and why>

<optional footer: BREAKING CHANGE: …, Closes #123>
```

Types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`,
`chore`. Scopes: `core`, `macros`, `query`, `model`, `db`, `deps`.
Breaking changes add `!` after the type (`feat(query)!: …`) and a
`BREAKING CHANGE:` footer explaining the migration.

## Pull request checklist

- [ ] CI checks pass locally (fmt, clippy, tests, docs)
- [ ] New behaviour has tests: SQL shape in `crates/rok-db/tests/sql.rs`,
      database behaviour in `crates/rok-db/tests/postgres.rs`
- [ ] Public items have rustdoc comments, with an example where it helps
- [ ] `CHANGELOG.md` has an entry under **Unreleased**
- [ ] README updated if user-facing behaviour changed

## Coding standards

- `rustfmt` defaults and zero clippy warnings.
- No `unsafe` code (enforced with `#![forbid(unsafe_code)]` via workspace lints).
- Every public item is documented (`missing_docs` is a warning, and CI
  denies warnings).
- Generated SQL must quote identifiers and bind every user-supplied value as
  a parameter. Never format values into SQL text.
- Futures returned by public APIs must be `Send`.
- Prefer adding to the builder API over adding new traits; keep the
  `prelude` small.

## Compatibility policy

- **Semantic Versioning.** rok-db follows [SemVer](https://semver.org/).
  While the version is `0.y.z`, a bump of `y` may contain breaking changes and
  a bump of `z` may not.
- **Lockstep versions.** `rok-db`, `rok-db-core` and `rok-db-macros` always
  share one version number. Only `rok-db` is a supported public API;
  `rok-db-core` and `rok-db-macros` items not re-exported by `rok-db`, and
  anything `#[doc(hidden)]` or named `__private`, may change at any time.
- **MSRV.** The minimum supported Rust version is the `rust-version` in
  `Cargo.toml` (currently 1.85) and is tested in CI. Raising it is a minor
  version bump, is noted in the changelog and only happens for a concrete
  benefit. The dependency resolver is configured (`.cargo/config.toml`) to
  pick versions compatible with the MSRV.
- **Deprecation.** Before removing a public item, deprecate it for at least
  one minor release with `#[deprecated(note = "use … instead")]`.
- **Database support.** We test against the PostgreSQL versions that are still
  [supported upstream](https://www.postgresql.org/support/versioning/).

## Release process

Maintainers cut releases from `main`:

1. Make sure CI on `main` is green.
2. Move the **Unreleased** entries in `CHANGELOG.md` under a new
   `## [x.y.z] - YYYY-MM-DD` heading and update the comparison links.
3. Bump `version` in `[workspace.package]` and in the
   `[workspace.dependencies]` entries of the root `Cargo.toml`.
4. Commit as `chore(release): vX.Y.Z`, then tag `vX.Y.Z` and push the tag.
5. Run `cargo package --workspace` (a dry run that builds and verifies all four crates).
6. Publish in dependency order:
   `cargo publish -p rok-db-macros && cargo publish -p rok-db-core && cargo publish -p rok-db && cargo publish -p rok-db-codegen`.
7. Create a GitHub release from the tag using the changelog entry.

## Coding agents

`AGENTS.md` (also loaded as `CLAUDE.md`) points coding agents at `llms.txt`, a digest of the
API and conventions, and at task checklists in `.agents/skills/` (`.claude` links to it):
checks, commits, pull requests, RFCs, the derive, query building, code-quality review and
releases. Keep them current when you change a convention.

## License

Unless you explicitly state otherwise, any contribution you intentionally
submit for inclusion in rok-db, as defined in the Apache-2.0 license, shall be
dual licensed as MIT OR Apache-2.0, without any additional terms or
conditions.
