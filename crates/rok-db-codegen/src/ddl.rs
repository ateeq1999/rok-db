//! Migrations: the SQL that turns one schema into another, with its undo.
//!
//! The first migration is the difference from an empty schema. Tables are
//! created without foreign keys, which are added once every table exists, so
//! reference cycles need no special order.

use crate::ir::{Check, Column, EnumType, Extra, ForeignKey, Schema, Table, Unique};
use crate::naming;
use crate::parse::quote;

/// One step of a migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Op {
    pub(crate) up: String,
    /// Empty when the step can't be undone.
    pub(crate) down: String,
    /// Why the step may lose data or fail on existing rows.
    pub(crate) destructive: Option<String>,
}

impl Op {
    fn new(up: impl Into<String>, down: impl Into<String>) -> Self {
        Self {
            up: up.into(),
            down: down.into(),
            destructive: None,
        }
    }

    fn destructive(mut self, why: impl Into<String>) -> Self {
        self.destructive = Some(why.into());
        self
    }
}

/// A rename the user asked for (`--rename`), applied before diffing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rename {
    /// `old=new` for a table.
    Table {
        /// Previous name (schema-qualified when the table is).
        from: String,
        /// New name, without a schema.
        to: String,
    },
    /// `table.old=new` for a column.
    Column {
        /// The table, by its previous name.
        table: String,
        /// Previous column name.
        from: String,
        /// New column name.
        to: String,
    },
}

impl Rename {
    /// Parse `old=new` or `table.old=new`.
    pub fn parse(spec: &str) -> Option<Self> {
        let (from, to) = spec.split_once('=')?;
        let (from, to) = (from.trim(), to.trim());
        if from.is_empty() || to.is_empty() || to.contains('.') {
            return None;
        }
        Some(match from.rsplit_once('.') {
            Some((table, column)) => Rename::Column {
                table: table.to_owned(),
                from: column.to_owned(),
                to: to.to_owned(),
            },
            None => Rename::Table {
                from: from.to_owned(),
                to: to.to_owned(),
            },
        })
    }
}

/// The steps from `old` to `new`, in an order that works on a live
/// database; their undo runs in reverse.
pub(crate) fn diff(old: &Schema, new: &Schema, renames: &[Rename]) -> Result<Vec<Op>, String> {
    let mut old = old.clone();
    let mut ops = Vec::new();

    // Removed extras go first (views may depend on columns that change).
    for extra in old.extras.iter().rev().filter(|e| !has_extra(new, e)) {
        ops.push(Op::new(&extra.down, &extra.up).destructive_if(
            extra.down.trim().is_empty(),
            "removes a statement that has no down SQL",
        ));
    }
    for extra in new
        .extras
        .iter()
        .filter(|e| e.before_tables && !has_extra(&old, e))
    {
        ops.push(Op::new(&extra.up, &extra.down));
    }

    // Enums.
    for e in &new.enums {
        match old.enums.iter().find(|o| o.name == e.name) {
            None => ops.push(Op::new(
                create_enum(e),
                format!("DROP TYPE {}", quote(&e.name)),
            )),
            Some(o) => ops.extend(enum_changes(o, e)?),
        }
    }

    // Renames.
    for rename in renames {
        ops.push(apply_rename(&mut old, rename)?);
    }

    let enum_names: Vec<&str> = new
        .enums
        .iter()
        .chain(&old.enums)
        .map(|e| e.name.as_str())
        .collect();
    let ty = |sql_type: &str| render_type(sql_type, &enum_names);

    let mut drop_constraints = Vec::new();
    let mut table_changes = Vec::new();
    let mut add_constraints = Vec::new();
    let mut add_fks = Vec::new();

    for table in &new.tables {
        let Some(before) = old.tables.iter().find(|t| t.name == table.name) else {
            table_changes.push(Op::new(
                create_table(table, &ty),
                format!("DROP TABLE {}", quote(&table.name)),
            ));
            for fk in &table.foreign_keys {
                add_fks.push(add_fk(table, fk));
            }
            continue;
        };
        let t = quote(&table.name);
        // Constraints that went away or changed are dropped first.
        if before.primary_key != table.primary_key {
            if !before.primary_key.is_empty() {
                drop_constraints.push(
                    Op::new(
                        format!("ALTER TABLE {t} DROP CONSTRAINT {}", quote(&pkey(before))),
                        format!(
                            "ALTER TABLE {t} ADD CONSTRAINT {} PRIMARY KEY ({})",
                            quote(&pkey(before)),
                            columns(&before.primary_key)
                        ),
                    )
                    .destructive(format!("changes the primary key of `{}`", table.name)),
                );
            }
            if !table.primary_key.is_empty() {
                add_constraints.push(Op::new(
                    format!(
                        "ALTER TABLE {t} ADD CONSTRAINT {} PRIMARY KEY ({})",
                        quote(&pkey(table)),
                        columns(&table.primary_key)
                    ),
                    format!("ALTER TABLE {t} DROP CONSTRAINT {}", quote(&pkey(table))),
                ));
            }
        }
        for fk in before
            .foreign_keys
            .iter()
            .filter(|fk| !table.foreign_keys.contains(fk))
        {
            let mut op = add_fk(before, fk);
            std::mem::swap(&mut op.up, &mut op.down);
            drop_constraints.push(op);
        }
        for fk in table
            .foreign_keys
            .iter()
            .filter(|fk| !before.foreign_keys.contains(fk))
        {
            add_fks.push(add_fk(table, fk));
        }
        for u in before.uniques.iter().filter(|u| !table.uniques.contains(u)) {
            let mut op = add_unique(before, u);
            std::mem::swap(&mut op.up, &mut op.down);
            drop_constraints.push(op);
        }
        for u in table.uniques.iter().filter(|u| !before.uniques.contains(u)) {
            add_constraints.push(add_unique(table, u));
        }
        for c in before.checks.iter().filter(|c| !table.checks.contains(c)) {
            let mut op = add_check(before, c);
            std::mem::swap(&mut op.up, &mut op.down);
            drop_constraints.push(op);
        }
        for c in table.checks.iter().filter(|c| !before.checks.contains(c)) {
            add_constraints.push(add_check(table, c));
        }
        // Columns.
        for column in before
            .columns
            .iter()
            .filter(|c| table.column(&c.name).is_none())
        {
            table_changes.push(
                Op::new(
                    format!("ALTER TABLE {t} DROP COLUMN {}", quote(&column.name)),
                    format!("ALTER TABLE {t} ADD COLUMN {}", column_def(column, &ty)),
                )
                .destructive(format!(
                    "drops column `{}.{}` and its data",
                    table.name, column.name
                )),
            );
        }
        for column in &table.columns {
            match before.column(&column.name) {
                None => {
                    let op = Op::new(
                        format!("ALTER TABLE {t} ADD COLUMN {}", column_def(column, &ty)),
                        format!("ALTER TABLE {t} DROP COLUMN {}", quote(&column.name)),
                    );
                    let needs_value = !column.nullable && !column.has_database_value();
                    table_changes.push(op.destructive_if(
                        needs_value,
                        format!(
                            "adds NOT NULL column `{}.{}` without a DEFAULT, which fails when the table has rows",
                            table.name, column.name
                        ),
                    ));
                }
                Some(old_column) => {
                    table_changes.extend(column_changes(table, old_column, column, &ty)?)
                }
            }
        }
    }

    // Indexes: dropped before columns change, created after.
    let mut drop_indexes = Vec::new();
    let mut create_indexes = Vec::new();
    for index in &old.indexes {
        if !new.indexes.contains(index) {
            drop_indexes.push(Op::new(
                format!(
                    "DROP INDEX IF EXISTS {}",
                    quote(&index_name(&index.table, &index.name))
                ),
                index.sql.clone(),
            ));
        }
    }
    for index in &new.indexes {
        if !old.indexes.contains(index) {
            create_indexes.push(Op::new(
                index.sql.clone(),
                format!(
                    "DROP INDEX IF EXISTS {}",
                    quote(&index_name(&index.table, &index.name))
                ),
            ));
        }
    }

    // Removed tables: their foreign keys first, then the tables.
    let removed: Vec<&Table> = old
        .tables
        .iter()
        .filter(|o| !new.tables.iter().any(|t| t.name == o.name))
        .collect();
    let mut drop_tables = Vec::new();
    for table in &removed {
        for fk in &table.foreign_keys {
            let mut op = add_fk(table, fk);
            std::mem::swap(&mut op.up, &mut op.down);
            drop_constraints.push(op);
        }
    }
    for table in removed.iter().rev() {
        drop_tables.push(
            Op::new(
                format!("DROP TABLE {}", quote(&table.name)),
                create_table(table, &ty),
            )
            .destructive(format!("drops table `{}` and its data", table.name)),
        );
    }

    ops.extend(drop_constraints);
    ops.extend(drop_indexes);
    ops.extend(table_changes);
    ops.extend(add_constraints);
    ops.extend(add_fks);
    ops.extend(create_indexes);
    ops.extend(drop_tables);

    for e in old
        .enums
        .iter()
        .filter(|o| !new.enums.iter().any(|e| e.name == o.name))
    {
        ops.push(
            Op::new(format!("DROP TYPE {}", quote(&e.name)), create_enum(e))
                .destructive(format!("drops enum type `{}`", e.name)),
        );
    }
    for extra in new
        .extras
        .iter()
        .filter(|e| !e.before_tables && !has_extra(&old, e))
    {
        ops.push(Op::new(&extra.up, &extra.down));
    }
    Ok(ops)
}

impl Op {
    fn destructive_if(self, condition: bool, why: impl Into<String>) -> Self {
        if condition {
            self.destructive(why)
        } else {
            self
        }
    }
}

fn has_extra(schema: &Schema, extra: &Extra) -> bool {
    schema.extras.iter().any(|e| e.up == extra.up)
}

fn enum_changes(old: &EnumType, new: &EnumType) -> Result<Vec<Op>, String> {
    let kept: Vec<&String> = new
        .values
        .iter()
        .filter(|v| old.values.contains(v))
        .collect();
    if kept.len() != old.values.len() || kept.iter().zip(&old.values).any(|(a, b)| *a != b) {
        return Err(format!(
            "enum `{}` lost or reordered values; PostgreSQL can only add values. \
             Write that change as a hand-made migration",
            old.name
        ));
    }
    let mut ops = Vec::new();
    for (i, value) in new.values.iter().enumerate() {
        if old.values.contains(value) {
            continue;
        }
        let position = match new.values[..i]
            .iter()
            .rev()
            .find(|v| old.values.contains(v))
        {
            Some(prev) => format!(" AFTER {}", literal(prev)),
            None => match new.values[i + 1..].iter().find(|v| old.values.contains(v)) {
                Some(next) => format!(" BEFORE {}", literal(next)),
                None => String::new(),
            },
        };
        // PostgreSQL can't remove an enum value, so this step has no undo.
        ops.push(Op::new(
            format!(
                "ALTER TYPE {} ADD VALUE {}{position}",
                quote(&new.name),
                literal(value)
            ),
            "",
        ));
    }
    Ok(ops)
}

fn column_changes(
    table: &Table,
    old: &Column,
    new: &Column,
    ty: &dyn Fn(&str) -> String,
) -> Result<Vec<Op>, String> {
    let t = quote(&table.name);
    let c = quote(&new.name);
    let mut ops = Vec::new();
    if old.generated != new.generated || old.identity != new.identity {
        return Err(format!(
            "`{}.{}` changed its GENERATED / IDENTITY definition; write that change as a hand-made migration",
            table.name, new.name
        ));
    }
    if old.sql_type != new.sql_type {
        ops.push(
            Op::new(
                format!(
                    "ALTER TABLE {t} ALTER COLUMN {c} TYPE {} USING {c}::{}",
                    ty(&new.sql_type),
                    ty(&new.sql_type)
                ),
                format!(
                    "ALTER TABLE {t} ALTER COLUMN {c} TYPE {} USING {c}::{}",
                    ty(&old.sql_type),
                    ty(&old.sql_type)
                ),
            )
            .destructive(format!(
                "changes the type of `{}.{}` from {} to {}",
                table.name, new.name, old.sql_type, new.sql_type
            )),
        );
    }
    if old.default != new.default {
        let set = |d: &Option<String>| match d {
            Some(d) => format!("ALTER TABLE {t} ALTER COLUMN {c} SET DEFAULT {d}"),
            None => format!("ALTER TABLE {t} ALTER COLUMN {c} DROP DEFAULT"),
        };
        ops.push(Op::new(set(&new.default), set(&old.default)));
    }
    if old.nullable != new.nullable {
        let set = format!("ALTER TABLE {t} ALTER COLUMN {c} SET NOT NULL");
        let drop = format!("ALTER TABLE {t} ALTER COLUMN {c} DROP NOT NULL");
        ops.push(if new.nullable {
            Op::new(drop, set)
        } else {
            Op::new(set, drop).destructive(format!(
                "makes `{}.{}` NOT NULL, which fails when it holds NULLs",
                table.name, new.name
            ))
        });
    }
    Ok(ops)
}

fn apply_rename(old: &mut Schema, rename: &Rename) -> Result<Op, String> {
    match rename {
        Rename::Table { from, to } => {
            let schema_prefix = from.rsplit_once('.').map(|(s, _)| format!("{s}."));
            let new_name = format!("{}{to}", schema_prefix.unwrap_or_default());
            let table = old
                .tables
                .iter_mut()
                .find(|t| &t.name == from)
                .ok_or_else(|| format!("--rename: the previous schema has no table `{from}`"))?;
            table.name.clone_from(&new_name);
            for t in &mut old.tables {
                for fk in &mut t.foreign_keys {
                    if &fk.ref_table == from {
                        fk.ref_table.clone_from(&new_name);
                    }
                }
            }
            for index in &mut old.indexes {
                if &index.table == from {
                    index.table.clone_from(&new_name);
                }
            }
            Ok(Op::new(
                format!("ALTER TABLE {} RENAME TO {}", quote(from), quote(to)),
                format!(
                    "ALTER TABLE {} RENAME TO {}",
                    quote(&new_name),
                    quote(naming::unqualified(from))
                ),
            ))
        }
        Rename::Column { table, from, to } => {
            let t = old
                .tables
                .iter_mut()
                .find(|t| &t.name == table)
                .ok_or_else(|| format!("--rename: the previous schema has no table `{table}`"))?;
            let column = t
                .columns
                .iter_mut()
                .find(|c| &c.name == from)
                .ok_or_else(|| format!("--rename: `{table}` has no column `{from}`"))?;
            column.name.clone_from(to);
            let rename = |cols: &mut Vec<String>| {
                for c in cols.iter_mut().filter(|c| *c == from) {
                    c.clone_from(to);
                }
            };
            rename(&mut t.primary_key);
            t.uniques.iter_mut().for_each(|u| rename(&mut u.columns));
            t.foreign_keys
                .iter_mut()
                .for_each(|fk| rename(&mut fk.columns));
            let table_name = t.name.clone();
            for other in &mut old.tables {
                for fk in other
                    .foreign_keys
                    .iter_mut()
                    .filter(|fk| fk.ref_table == table_name)
                {
                    rename(&mut fk.ref_columns);
                }
            }
            Ok(Op::new(
                format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {}",
                    quote(table),
                    quote(from),
                    quote(to)
                ),
                format!(
                    "ALTER TABLE {} RENAME COLUMN {} TO {}",
                    quote(table),
                    quote(to),
                    quote(from)
                ),
            ))
        }
    }
}

pub(crate) fn create_enum(e: &EnumType) -> String {
    let values: Vec<String> = e.values.iter().map(|v| literal(v)).collect();
    format!(
        "CREATE TYPE {} AS ENUM ({})",
        quote(&e.name),
        values.join(", ")
    )
}

/// `CREATE TABLE` without foreign keys (they are added separately).
pub(crate) fn create_table(table: &Table, ty: &dyn Fn(&str) -> String) -> String {
    let mut lines: Vec<String> = table.columns.iter().map(|c| column_def(c, ty)).collect();
    if !table.primary_key.is_empty() {
        lines.push(format!(
            "CONSTRAINT {} PRIMARY KEY ({})",
            quote(&pkey(table)),
            columns(&table.primary_key)
        ));
    }
    for u in &table.uniques {
        lines.push(format!(
            "CONSTRAINT {} UNIQUE ({})",
            quote(&u.name),
            columns(&u.columns)
        ));
    }
    for c in &table.checks {
        lines.push(format!("CONSTRAINT {} CHECK ({})", quote(&c.name), c.expr));
    }
    format!(
        "CREATE TABLE {} (\n    {}\n)",
        quote(&table.name),
        lines.join(",\n    ")
    )
}

fn column_def(c: &Column, ty: &dyn Fn(&str) -> String) -> String {
    let mut def = format!("{} {}", quote(&c.name), ty(&c.sql_type));
    if let Some(expr) = &c.generated {
        def.push_str(&format!(" GENERATED ALWAYS AS ({expr}) STORED"));
    }
    if let Some(kind) = &c.identity {
        def.push_str(&format!(" GENERATED {kind} AS IDENTITY"));
    }
    if !c.nullable {
        def.push_str(" NOT NULL");
    }
    if let Some(d) = &c.default {
        def.push_str(&format!(" DEFAULT {d}"));
    }
    def
}

fn add_fk(table: &Table, fk: &ForeignKey) -> Op {
    let t = quote(&table.name);
    let mut up = format!(
        "ALTER TABLE {t} ADD CONSTRAINT {} FOREIGN KEY ({}) REFERENCES {} ({})",
        quote(&fk.name),
        columns(&fk.columns),
        quote(&fk.ref_table),
        columns(&fk.ref_columns)
    );
    if let Some(action) = &fk.on_delete {
        up.push_str(&format!(" ON DELETE {action}"));
    }
    if let Some(action) = &fk.on_update {
        up.push_str(&format!(" ON UPDATE {action}"));
    }
    Op::new(
        up,
        format!("ALTER TABLE {t} DROP CONSTRAINT {}", quote(&fk.name)),
    )
}

fn add_unique(table: &Table, u: &Unique) -> Op {
    let t = quote(&table.name);
    Op::new(
        format!(
            "ALTER TABLE {t} ADD CONSTRAINT {} UNIQUE ({})",
            quote(&u.name),
            columns(&u.columns)
        ),
        format!("ALTER TABLE {t} DROP CONSTRAINT {}", quote(&u.name)),
    )
}

fn add_check(table: &Table, c: &Check) -> Op {
    let t = quote(&table.name);
    Op::new(
        format!(
            "ALTER TABLE {t} ADD CONSTRAINT {} CHECK ({})",
            quote(&c.name),
            c.expr
        ),
        format!("ALTER TABLE {t} DROP CONSTRAINT {}", quote(&c.name)),
    )
}

fn pkey(table: &Table) -> String {
    format!("{}_pkey", naming::unqualified(&table.name))
}

/// An index lives in its table's schema.
fn index_name(table: &str, index: &str) -> String {
    match table.rsplit_once('.') {
        Some((schema, _)) if !index.contains('.') => format!("{schema}.{index}"),
        _ => index.to_owned(),
    }
}

fn columns(cols: &[String]) -> String {
    cols.iter().map(|c| quote(c)).collect::<Vec<_>>().join(", ")
}

fn render_type(sql_type: &str, enums: &[&str]) -> String {
    let (base, dims) = match sql_type.find("[]") {
        Some(i) => (&sql_type[..i], &sql_type[i..]),
        None => (sql_type, ""),
    };
    if enums.contains(&base) {
        format!("{}{dims}", quote(base))
    } else {
        sql_type.to_owned()
    }
}

/// A SQL string literal.
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Join steps into one migration's `up` and `down` scripts. `down` is empty
/// when a step can't be undone.
pub(crate) fn scripts(ops: &[Op]) -> (String, String) {
    let up = ops
        .iter()
        .map(|op| format!("{};\n", op.up))
        .collect::<String>();
    let down = if ops.iter().any(|op| op.down.trim().is_empty()) {
        String::new()
    } else {
        ops.iter()
            .rev()
            .map(|op| format!("{};\n", op.down))
            .collect()
    };
    (up, down)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{SourceFile, parse};

    fn schema(text: &str) -> Schema {
        parse(&[SourceFile {
            path: "db/app.sql".into(),
            module: "app".into(),
            text: text.into(),
        }])
        .unwrap()
        .schema
    }

    const V1: &str = "
        CREATE TYPE role AS ENUM ('admin', 'member');
        CREATE TABLE users (id BIGSERIAL PRIMARY KEY, email TEXT NOT NULL UNIQUE, role role NOT NULL DEFAULT 'member');
        CREATE TABLE posts (id BIGSERIAL PRIMARY KEY, user_id BIGINT NOT NULL REFERENCES users, title TEXT NOT NULL);
        CREATE INDEX posts_user_idx ON posts (user_id);";

    #[test]
    fn init_creates_everything_and_undoes_it() {
        let ops = diff(&Schema::default(), &schema(V1), &[]).unwrap();
        let (up, down) = scripts(&ops);
        let create_users = up.find("CREATE TABLE \"users\"").unwrap();
        let fk = up
            .find("FOREIGN KEY (\"user_id\") REFERENCES \"users\" (\"id\")")
            .unwrap();
        assert!(up.starts_with("CREATE TYPE \"role\" AS ENUM ('admin', 'member');"));
        assert!(create_users < fk, "{up}");
        assert!(
            up.contains("\"role\" \"role\" NOT NULL DEFAULT 'member'"),
            "{up}"
        );
        assert!(
            up.ends_with("CREATE INDEX posts_user_idx ON posts(user_id);\n"),
            "{up}"
        );
        assert!(
            down.starts_with("DROP INDEX IF EXISTS \"posts_user_idx\";"),
            "{down}"
        );
        assert!(down.ends_with("DROP TYPE \"role\";\n"), "{down}");
        assert!(ops.iter().all(|op| op.destructive.is_none()));
    }

    #[test]
    fn no_changes_no_steps() {
        assert!(diff(&schema(V1), &schema(V1), &[]).unwrap().is_empty());
    }

    #[test]
    fn column_and_enum_changes() {
        let v2 = V1
            .replace("'admin', 'member'", "'admin', 'editor', 'member'")
            .replace(
                "title TEXT NOT NULL",
                "title TEXT, slug TEXT NOT NULL DEFAULT ''",
            );
        let ops = diff(&schema(V1), &schema(&v2), &[]).unwrap();
        let ups: Vec<&str> = ops.iter().map(|op| op.up.as_str()).collect();
        assert_eq!(
            ups,
            [
                "ALTER TYPE \"role\" ADD VALUE 'editor' AFTER 'admin'",
                "ALTER TABLE \"posts\" ALTER COLUMN \"title\" DROP NOT NULL",
                "ALTER TABLE \"posts\" ADD COLUMN \"slug\" TEXT NOT NULL DEFAULT ''",
            ]
        );
        // Adding an enum value can't be undone.
        assert!(scripts(&ops).1.is_empty());
    }

    #[test]
    fn destructive_steps_are_flagged() {
        let v2 = V1.replace(", title TEXT NOT NULL", ", body TEXT NOT NULL");
        let ops = diff(&schema(V1), &schema(&v2), &[]).unwrap();
        let reasons: Vec<&str> = ops
            .iter()
            .filter_map(|op| op.destructive.as_deref())
            .collect();
        assert_eq!(reasons.len(), 2, "{reasons:?}");
        assert!(reasons[0].contains("drops column `posts.title`"));
        assert!(reasons[1].contains("without a DEFAULT"));

        // ...unless it is a rename.
        let rename = [Rename::parse("posts.title=body").unwrap()];
        let ops = diff(&schema(V1), &schema(&v2), &rename).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0].up,
            "ALTER TABLE \"posts\" RENAME COLUMN \"title\" TO \"body\""
        );
    }

    #[test]
    fn removed_enum_values_are_refused() {
        let v2 = V1.replace("'admin', 'member'", "'member'");
        assert!(
            diff(&schema(V1), &schema(&v2), &[])
                .unwrap_err()
                .contains("lost or reordered")
        );
    }
}
