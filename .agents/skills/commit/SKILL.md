---
name: commit
description: Write a Conventional Commits message with a rok-db scope.
---

# Commit messages

Format: `type(scope): summary` in the imperative, lower case, no trailing period, at most 72
characters. The body says what and why, wrapped at 72 columns.

Types: `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`.
Breaking changes add `!` (`feat(query)!: ...`) and a `BREAKING CHANGE:` footer with the
migration.

Scopes: `core`, `macros`, `query`, `model`, `db`, `deps`, or a feature area when it reads
better (`joins`, `cache`, `tenant`, `audit`, `rfc`). Leave the scope out for changes that span
many areas.

Maintainers squash-merge, so the pull request title becomes the commit on `main` and must
follow the same format.
