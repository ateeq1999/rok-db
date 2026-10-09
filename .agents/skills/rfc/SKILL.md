---
name: rfc
description: When and how to write a rok-db RFC (public API, new crates or features, dependencies, MSRV bumps, new databases, breaking changes).
---

# RFCs

Needed for: public API additions beyond small builder methods, breaking changes, new crates or
Cargo features, new dependencies, MSRV bumps and new supported databases. Bug fixes, docs and
internal refactors don't need one.

Steps:

1. Copy `docs/rfcs/0000-template.md` to `docs/rfcs/NNNN-short-name.md` with the next free
   number. Status: **Draft**.
2. Fill in every section: Summary, Motivation, Guide-level explanation (code a user would
   write), Reference-level explanation (types, rendered SQL, interaction with scopes,
   tenancy, soft deletes, caching and replicas), Drawbacks, Rationale and alternatives, Prior
   art (Diesel, SeaORM, sqlx, ActiveRecord, Prisma), Compatibility, Unresolved questions,
   Future possibilities.
3. List open questions with a recommended answer each, so the maintainer can approve quickly.
4. Do not implement until the maintainer accepts it. Then set Status to **Accepted** with the
   date, record the decisions in a "Decisions" section, and describe the shipped scope.
5. When implementation changes the design, amend the RFC in the same pull request.

Existing RFCs: `0001-joins` (accepted), `0002-cli` (accepted), `0003-multiple-databases`
(draft), `0004-joined-tuples` (accepted), `0005-numeric-and-interval` (accepted).
