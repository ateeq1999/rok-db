---
name: release
description: How rok-db versions and publishes its four crates (automated on merge to main).
---

# Releases

`rok-db`, `rok-db-core`, `rok-db-macros` and `rok-db-codegen` share one version (lockstep, SemVer; while `0.y.z`,
a `y` bump may break).

Publishing is automated by `.github/workflows/release.yml`. After CI passes on `main`, it
publishes every crate whose workspace version is not on crates.io yet (with the
`CARGO_REGISTRY_TOKEN` secret), then tags `vX.Y.Z` and creates the GitHub release from the
changelog entry. A merge that doesn't bump the version publishes nothing.

To release, open a pull request that:

1. Moves the **Unreleased** entries in `CHANGELOG.md` under `## [x.y.z] - YYYY-MM-DD` and
   updates the comparison links.
2. Bumps `version` in `[workspace.package]` and in the `[workspace.dependencies]` entries of
   the root `Cargo.toml`.
3. Passes `cargo package --workspace` from a clean checkout (each crate must list its
   README, `LICENSE-MIT` and `LICENSE-APACHE`; the root `.gitignore` ignores `*.md` outside
   listed paths).
4. Is committed as `chore(release): vX.Y.Z`.

Merging it publishes. Never publish from a branch: a crates.io version can't be replaced.
If a run fails partway, fix the cause and re-run the workflow (or run it by hand from the
Actions tab); crates already published are skipped. The manual fallback, in dependency
order, is
`cargo publish -p rok-db-macros && cargo publish -p rok-db-core && cargo publish -p rok-db && cargo publish -p rok-db-codegen`.

Downstream: rok-ui depends on rok-db from crates.io (`rok-db = "0.1"` as of rok-ui 0.6). After
a release with features rok-ui should use, bump its requirement and update its `db` skill and
database guide.
