---
name: release
description: How rok-db versions and publishes its three crates.
---

# Releases

`rok-db`, `rok-db-core`, `rok-db-macros` and `rok-db-codegen` share one version (lockstep, SemVer; while `0.y.z`,
a `y` bump may break).

1. CI on `main` is green.
2. Move the **Unreleased** entries in `CHANGELOG.md` under `## [x.y.z] - YYYY-MM-DD` and
   update the comparison links.
3. Bump `version` in `[workspace.package]` and in the `[workspace.dependencies]` entries of
   the root `Cargo.toml`.
4. Commit `chore(release): vX.Y.Z`, tag `vX.Y.Z`, push the tag.
5. Dry run: `cargo package --workspace` builds and verifies all three crates (each must list
   `LICENSE-MIT` and `LICENSE-APACHE`). Publish from a checkout with LF line endings
   (`.gitattributes` enforces it).
6. Publish in dependency order:
   `cargo publish -p rok-db-macros && cargo publish -p rok-db-core && cargo publish -p rok-db && cargo publish -p rok-db-codegen`.
7. Create the GitHub release from the changelog entry.

Downstream: rok-ui depends on rok-db from crates.io (`rok-db = "0.1"` as of rok-ui 0.6). After
a release with features rok-ui should use, bump its requirement and update its `db` skill and
database guide.
