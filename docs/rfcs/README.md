# rok-db RFCs

An RFC ("request for comments") is how substantial changes are proposed,
discussed and approved before code is written.

## When you need an RFC

- New public API (types, traits, builder methods with new concepts)
- Breaking changes or deprecations
- New Cargo features, crates, or dependencies
- Support for a new database backend
- Raising the MSRV
- Changes to [governance](../../GOVERNANCE.md)

Bug fixes, docs, tests, internal refactors and small additions that follow
existing patterns (for example a new column operator) only need an issue.

## Lifecycle

1. **Draft** — copy [`0000-template.md`](0000-template.md) to
   `docs/rfcs/0000-short-name.md`, fill it in and open a pull request titled
   `rfc: <short name>`. Rename the file using the pull request number.
2. **Discussion** — anyone may comment. The author updates the RFC in place.
3. **Final comment period (FCP)** — when discussion settles, a maintainer
   proposes *merge*, *close* or *postpone*. The FCP lasts at least 7 days.
4. **Decision**
   - *Accepted*: the RFC pull request is merged, a tracking issue labelled
     `rfc-accepted` is opened, and implementation may start.
   - *Postponed*: the pull request is closed with the `postponed` label and
     may be reopened later.
   - *Rejected*: the pull request is closed with the reasoning recorded.
5. **Implementation** — pull requests reference the tracking issue. Changes
   to an accepted design are made by amending the RFC.

An accepted RFC is a design agreement, not a deadline; anyone may implement it.
