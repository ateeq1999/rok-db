---
name: codegen
description: Change rok-db-gen (crates/rok-db-codegen), which turns .sql files into a generated crate, or the migration runner (rok_db::migration).
---

# SQL code generation

The design and the user-facing reference are in `docs/v4.md`. Keep that file in sync with
behaviour changes.

Pipeline (`crates/rok-db-codegen/src`):

| File | Job |
|---|---|
| `split.rs` | split a file into statements, keeping the comment block above each one |
| `parse.rs` | statements to the schema model (`ir.rs`) and the annotated queries |
| `types.rs` | canonical SQL type spellings, SQL to Rust types, required rok-db features |
| `ddl.rs` | render SQL from the model, and diff two schemas into migration steps |
| `describe.rs` | PostgreSQL types of queries (temporary database) and the `queries.json` cache |
| `queries.rs` | parameter names, written tables, return types, generated functions |
| `emit.rs` | models, enums, relations, formatting (prettyplease) |
| `generate.rs` | the whole run: files in memory, `write`, `differences` (check) |
| `main.rs` | the CLI and `rok-db.toml` |

Rules:

- Output must be deterministic and independent of the working directory: no absolute
  paths or timestamps, and stable ordering. `rok-db-gen check` compares bytes.
- Generated code only uses rok-db's public API (`rok_db::...`, `rok_db::sqlx::types::...`)
  so the generated crate depends on rok-db alone.
- Never rewrite an existing `migrations/*.sql` file: it is history and may be applied
  somewhere. Changes go into a new migration.
- Every migration step has undo SQL. A step that can't be undone (`ALTER TYPE ... ADD
  VALUE`) leaves the migration without a down file. Anything that loses data sets
  `destructive` with a reason.
- Unsupported SQL is an error that names the file and line and says what to do. It never
  generates something silently wrong.

After a change:

1. Regenerate the example and commit it with the change:
   `DATABASE_URL=... cargo run -p rok-db-codegen -- generate --config examples/sqlgen/rok-db.toml`
2. Run `cargo test -p rok-db-codegen` with `DATABASE_URL` set. That runs the unit tests, the
   golden test (`committed_example_is_up_to_date`), the PostgreSQL test of the generated code,
   and the migration workflow test.
3. For runtime changes: `cargo test -p rok-db --all-features --test migration`.
4. Check that no `rok_gen_*` temporary databases were left behind.
