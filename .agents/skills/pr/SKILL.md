---
name: pr
description: Describe a rok-db pull request.
---

# Pull requests

- Title: a Conventional Commits message (it becomes the squash-merge commit).
- Fill in `.github/PULL_REQUEST_TEMPLATE.md`: Summary, Type of change (link the accepted RFC
  or issue for features), Checklist, and Breaking changes / migration when the public API
  changes.
- Describe the whole diff, not only the last commit.
- Say how you tested: which checks from the `check` skill ran, and that the database tests ran
  against a real server (`DATABASE_URL` set).
- New behaviour has tests: SQL shape in `crates/rok-db/tests/sql.rs` (or the feature's test
  file, e.g. `joins.rs`) and database behaviour against Postgres.
- `CHANGELOG.md` has an entry under **Unreleased**; the README is updated for user-facing
  changes.
- If an AI assistant wrote part of the change, say so.
