//! Build the schema model and the query list from `.sql` files.

use sqlparser::ast::{
    ColumnOption, CommentObject, CreateTable, Expr, GeneratedAs, Ident, ObjectName, Statement,
    TableConstraint, UserDefinedTypeRepresentation,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::ir::{
    Check, Column, EnumType, Extra, ForeignKey, Index, Query, QueryKind, Schema, Table, Unique,
};
use crate::split::{RawStatement, split};
use crate::{Error, naming, types};

/// One input file.
#[derive(Debug, Clone)]
pub struct SourceFile {
    /// Path for messages (`db/user.sql`).
    pub path: String,
    /// Module name (from the file stem).
    pub module: String,
    /// File contents.
    pub text: String,
}

/// The parsed input: the schema and the annotated queries.
#[derive(Debug, Default)]
pub struct Parsed {
    /// Declared schema.
    pub schema: Schema,
    /// Annotated queries.
    pub queries: Vec<Query>,
    /// Modules, in file order (shared `_` files excluded).
    pub modules: Vec<String>,
}

/// Parse every file. Files whose stem starts with `_` hold shared
/// statements; their enums land in the `types` module.
pub fn parse(files: &[SourceFile]) -> Result<Parsed, Error> {
    let mut parsed = Parsed::default();
    let mut comments: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for file in files {
        let shared = file.module.starts_with('_');
        let module = if shared {
            "types".to_owned()
        } else {
            file.module.clone()
        };
        if !shared && !parsed.modules.contains(&module) {
            parsed.modules.push(module.clone());
        }
        let statements = split(&file.text);
        let mut i = 0;
        while i < statements.len() {
            let stmt = &statements[i];
            let at = format!("{}:{}", file.path, stmt.line);
            let directives = Directives::read(&stmt.comments, &at)?;
            if directives.down {
                return Err(Error::at(
                    &at,
                    "`-- rok:down` must directly follow the statement it undoes",
                ));
            }
            // A `-- rok:down` statement right after this one is its undo SQL.
            let explicit_down = match statements.get(i + 1) {
                Some(next) if Directives::read(&next.comments, &at)?.down => {
                    i += 1;
                    Some(next.sql.clone())
                }
                _ => None,
            };
            if let Some((name, kind)) = directives.query {
                if shared {
                    return Err(Error::at(&at, "queries can't live in a shared `_` file"));
                }
                parsed.queries.push(Query {
                    name,
                    module: module.clone(),
                    kind,
                    sql: stmt.sql.clone(),
                    doc: directives.doc,
                    param_names: directives.params,
                    location: at,
                });
                i += 1;
                continue;
            }
            schema_statement(
                stmt,
                &module,
                directives.doc,
                explicit_down,
                &at,
                &mut parsed.schema,
                &mut comments,
            )?;
            i += 1;
        }
    }
    apply_comments(&mut parsed.schema, &comments);
    resolve_references(&mut parsed.schema);
    check(&parsed)?;
    Ok(parsed)
}

/// Directives and docs from a statement's comment block.
#[derive(Default)]
struct Directives {
    query: Option<(String, QueryKind)>,
    params: Vec<(usize, String)>,
    down: bool,
    doc: Vec<String>,
}

impl Directives {
    fn read(comments: &[String], at: &str) -> Result<Self, Error> {
        let mut d = Directives::default();
        for line in comments {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("name:") {
                let mut parts = rest.split_whitespace();
                let (Some(name), Some(kind), None) = (parts.next(), parts.next(), parts.next())
                else {
                    return Err(Error::at(
                        at,
                        "expected `-- name: <function> <:one|:one!|:many|:stream|:exec>`",
                    ));
                };
                let kind = QueryKind::parse(kind).ok_or_else(|| {
                    Error::at(
                        at,
                        format!(
                            "unknown query kind `{kind}`; use :one, :one!, :many, :stream or :exec"
                        ),
                    )
                })?;
                if naming::snake_ident(name) != name {
                    return Err(Error::at(
                        at,
                        format!("query name `{name}` must be a snake_case Rust identifier"),
                    ));
                }
                d.query = Some((name.to_owned(), kind));
            } else if let Some(rest) = line.strip_prefix("param:") {
                let mut parts = rest.split_whitespace();
                let parsed = match (parts.next(), parts.next()) {
                    (Some(p), Some(name)) => p
                        .strip_prefix('$')
                        .and_then(|n| n.parse::<usize>().ok())
                        .map(|n| (n, naming::snake_ident(name))),
                    _ => None,
                };
                d.params
                    .push(parsed.ok_or_else(|| Error::at(at, "expected `-- param: $<n> <name>`"))?);
            } else if line == "rok:down" {
                d.down = true;
            } else if let Some(directive) = line.strip_prefix("rok:") {
                return Err(Error::at(
                    at,
                    format!("unknown directive `rok:{directive}` here"),
                ));
            } else {
                d.doc.push(line.to_owned());
            }
        }
        Ok(d)
    }
}

fn parse_one(sql: &str) -> Option<Statement> {
    let mut statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).ok()?;
    (statements.len() == 1).then(|| statements.remove(0))
}

fn schema_statement(
    stmt: &RawStatement,
    module: &str,
    doc: Vec<String>,
    explicit_down: Option<String>,
    at: &str,
    schema: &mut Schema,
    comments: &mut Vec<(String, Option<String>, Option<String>)>,
) -> Result<(), Error> {
    let parsed = parse_one(&stmt.sql);
    let looks_like_table = stmt
        .sql
        .split_whitespace()
        .take(2)
        .map(str::to_ascii_uppercase)
        .eq(["CREATE", "TABLE"]);
    match parsed {
        Some(Statement::CreateTable(table)) => {
            let table = table_from(&table, module, doc, &stmt.sql, at)?;
            if schema.tables.iter().any(|t| t.name == table.name) {
                return Err(Error::at(
                    at,
                    format!("table `{}` is declared twice", table.name),
                ));
            }
            schema.tables.push(table);
        }
        Some(Statement::CreateType {
            name,
            representation: Some(UserDefinedTypeRepresentation::Enum { labels }),
        }) => {
            schema.enums.push(EnumType {
                name: object_name(&name),
                values: labels.iter().map(|l| l.value.clone()).collect(),
                module: module.to_owned(),
                doc,
            });
        }
        Some(Statement::CreateIndex(index)) => {
            let Some(name) = &index.name else {
                return Err(Error::at(
                    at,
                    "give the index a name (`CREATE INDEX <name> ON ...`)",
                ));
            };
            schema.indexes.push(Index {
                name: object_name(name),
                table: object_name(&index.table_name),
                sql: Statement::CreateIndex(index.clone()).to_string(),
            });
        }
        Some(Statement::Comment {
            object_type,
            object_name: target,
            comment,
            ..
        }) if matches!(object_type, CommentObject::Table | CommentObject::Column) => {
            let target = object_name(&target);
            let (table, column) = match object_type {
                CommentObject::Column => match target.rsplit_once('.') {
                    Some((t, c)) => (t.to_owned(), Some(c.to_owned())),
                    None => return Err(Error::at(at, "expected `COMMENT ON COLUMN table.column`")),
                },
                _ => (target, None),
            };
            let what = if column.is_some() { "COLUMN" } else { "TABLE" };
            comments.push((table, column, comment.clone()));
            schema.extras.push(Extra {
                up: stmt.sql.clone(),
                down: format!("COMMENT ON {what} {} IS NULL", stmt_target(&stmt.sql)),
                before_tables: false,
            });
        }
        Some(Statement::CreateExtension(ext)) => schema.extras.push(Extra {
            up: stmt.sql.clone(),
            down: explicit_down
                .clone()
                .unwrap_or_else(|| format!("DROP EXTENSION IF EXISTS {}", quote(&ext.name.value))),
            before_tables: true,
        }),
        Some(Statement::CreateView(view)) if explicit_down.is_none() => schema.extras.push(Extra {
            up: stmt.sql.clone(),
            down: format!("DROP VIEW IF EXISTS {}", view.name),
            before_tables: false,
        }),
        None if looks_like_table => {
            return Err(Error::at(at, "couldn't parse this CREATE TABLE statement"));
        }
        _ => {
            let Some(down) = explicit_down else {
                return Err(Error::at(
                    at,
                    "rok-db-gen can't undo this statement by itself; follow it with \
                     `-- rok:down` and the SQL that undoes it",
                ));
            };
            schema.extras.push(Extra {
                up: stmt.sql.clone(),
                down,
                before_tables: false,
            });
        }
    }
    Ok(())
}

/// `COMMENT ON COLUMN users.email IS '...'` -> `users.email`.
fn stmt_target(sql: &str) -> String {
    let words: Vec<&str> = sql.split_whitespace().collect();
    words.get(3).copied().unwrap_or_default().to_owned()
}

fn table_from(
    table: &CreateTable,
    module: &str,
    doc: Vec<String>,
    sql: &str,
    at: &str,
) -> Result<Table, Error> {
    let name = object_name(&table.name);
    let short = naming::unqualified(&name).to_owned();
    let column_notes = column_comments(sql);
    let mut out = Table {
        name: name.clone(),
        module: module.to_owned(),
        doc,
        columns: Vec::new(),
        primary_key: Vec::new(),
        uniques: Vec::new(),
        foreign_keys: Vec::new(),
        checks: Vec::new(),
    };
    for def in &table.columns {
        let column_name = ident(&def.name);
        let mut column = Column {
            name: column_name.clone(),
            sql_type: types::canonical(&def.data_type.to_string()),
            nullable: true,
            default: None,
            generated: None,
            identity: None,
            tenant: false,
            doc: Vec::new(),
        };
        for option in &def.options {
            match &option.option {
                ColumnOption::NotNull => column.nullable = false,
                ColumnOption::Null => column.nullable = true,
                ColumnOption::Default(expr) => column.default = Some(expr.to_string()),
                ColumnOption::PrimaryKey(_) => {
                    if !out.primary_key.is_empty() {
                        return Err(Error::at(
                            at,
                            format!("table `{name}` has two primary keys"),
                        ));
                    }
                    out.primary_key = vec![column_name.clone()];
                }
                ColumnOption::Unique(_) => out.uniques.push(Unique {
                    name: format!("{short}_{column_name}_key"),
                    columns: vec![column_name.clone()],
                }),
                ColumnOption::ForeignKey(fk) => out.foreign_keys.push(ForeignKey {
                    name: constraint_name(fk.name.as_ref(), || {
                        format!("{short}_{column_name}_fkey")
                    }),
                    columns: vec![column_name.clone()],
                    ref_table: object_name(&fk.foreign_table),
                    ref_columns: fk.referred_columns.iter().map(ident).collect(),
                    on_delete: fk.on_delete.as_ref().map(ToString::to_string),
                    on_update: fk.on_update.as_ref().map(ToString::to_string),
                }),
                ColumnOption::Check(check) => out.checks.push(Check {
                    name: constraint_name(check.name.as_ref(), || {
                        format!("{short}_{column_name}_check")
                    }),
                    expr: check.expr.to_string(),
                }),
                ColumnOption::Generated {
                    generated_as,
                    generation_expr,
                    ..
                } => match generation_expr {
                    Some(expr) => column.generated = Some(expr.to_string()),
                    None => {
                        column.identity = Some(
                            match generated_as {
                                GeneratedAs::Always => "ALWAYS",
                                _ => "BY DEFAULT",
                            }
                            .to_owned(),
                        );
                    }
                },
                other => {
                    return Err(Error::at(
                        at,
                        format!("column option `{other}` on `{column_name}` is not supported"),
                    ));
                }
            }
        }
        if let Some((doc, tenant)) = column_notes
            .iter()
            .find_map(|(c, d, t)| (c.eq_ignore_ascii_case(&column_name)).then(|| (d.clone(), *t)))
        {
            column.doc = doc;
            column.tenant = tenant;
        }
        out.columns.push(column);
    }
    let mut unnamed_checks = 0;
    for constraint in &table.constraints {
        match constraint {
            TableConstraint::PrimaryKey(pk) => {
                if !out.primary_key.is_empty() {
                    return Err(Error::at(
                        at,
                        format!("table `{name}` has two primary keys"),
                    ));
                }
                out.primary_key = pk
                    .columns
                    .iter()
                    .map(|c| expr_ident(&c.column.expr))
                    .collect();
            }
            TableConstraint::Unique(u) => {
                let columns: Vec<String> = u
                    .columns
                    .iter()
                    .map(|c| expr_ident(&c.column.expr))
                    .collect();
                out.uniques.push(Unique {
                    name: constraint_name(u.name.as_ref(), || {
                        format!("{short}_{}_key", columns.join("_"))
                    }),
                    columns,
                });
            }
            TableConstraint::ForeignKey(fk) => {
                let columns: Vec<String> = fk.columns.iter().map(ident).collect();
                out.foreign_keys.push(ForeignKey {
                    name: constraint_name(fk.name.as_ref(), || {
                        format!("{short}_{}_fkey", columns.join("_"))
                    }),
                    columns,
                    ref_table: object_name(&fk.foreign_table),
                    ref_columns: fk.referred_columns.iter().map(ident).collect(),
                    on_delete: fk.on_delete.as_ref().map(ToString::to_string),
                    on_update: fk.on_update.as_ref().map(ToString::to_string),
                });
            }
            TableConstraint::Check(check) => {
                let name = constraint_name(check.name.as_ref(), || {
                    unnamed_checks += 1;
                    match unnamed_checks {
                        1 => format!("{short}_check"),
                        n => format!("{short}_check{}", n - 1),
                    }
                });
                out.checks.push(Check {
                    name,
                    expr: check.expr.to_string(),
                });
            }
            other => {
                return Err(Error::at(
                    at,
                    format!("table constraint `{other}` is not supported"),
                ));
            }
        }
    }
    // Primary key columns are NOT NULL.
    for column in &mut out.columns {
        if out.primary_key.contains(&column.name) {
            column.nullable = false;
        }
    }
    Ok(out)
}

/// `-- comment` lines above column definitions inside `CREATE TABLE`:
/// (column, doc lines, `rok:tenant`).
fn column_comments(sql: &str) -> Vec<(String, Vec<String>, bool)> {
    let mut out = Vec::new();
    let mut doc = Vec::new();
    let mut tenant = false;
    for line in sql.lines().skip(1) {
        let line = line.trim();
        if let Some(comment) = line.strip_prefix("--") {
            let comment = comment.trim();
            if comment == "rok:tenant" {
                tenant = true;
            } else {
                doc.push(comment.to_owned());
            }
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if !doc.is_empty() || tenant {
            let first = line
                .split(|c: char| c.is_whitespace() || c == ',')
                .next()
                .unwrap_or_default();
            let column = first.trim_matches('"').to_owned();
            out.push((column, std::mem::take(&mut doc), tenant));
            tenant = false;
        }
    }
    out
}

/// `REFERENCES users` without columns points at the primary key.
fn resolve_references(schema: &mut Schema) {
    let keys: Vec<(String, Vec<String>)> = schema
        .tables
        .iter()
        .map(|t| (t.name.clone(), t.primary_key.clone()))
        .collect();
    for table in &mut schema.tables {
        for fk in &mut table.foreign_keys {
            if fk.ref_columns.is_empty() {
                if let Some((_, key)) = keys.iter().find(|(name, _)| *name == fk.ref_table) {
                    fk.ref_columns = key.clone();
                }
            }
        }
    }
}

fn apply_comments(schema: &mut Schema, comments: &[(String, Option<String>, Option<String>)]) {
    for (table, column, text) in comments {
        let Some(t) = schema.tables.iter_mut().find(|t| &t.name == table) else {
            continue;
        };
        let lines: Vec<String> = text
            .iter()
            .flat_map(|t| t.lines())
            .map(str::to_owned)
            .collect();
        match column {
            Some(c) => {
                if let Some(col) = t.columns.iter_mut().find(|col| &col.name == c) {
                    col.doc = lines;
                }
            }
            None => t.doc = lines,
        }
    }
}

/// Cross-file checks: references and names.
fn check(parsed: &Parsed) -> Result<(), Error> {
    let schema = &parsed.schema;
    for module in &parsed.modules {
        if naming::RESERVED_MODULES.contains(&module.as_str()) {
            return Err(Error::new(format!(
                "`{module}.sql` would generate `{module}.rs`, which rok-db-gen writes itself; rename the file"
            )));
        }
    }
    for table in &schema.tables {
        for fk in &table.foreign_keys {
            let Some(target) = schema.tables.iter().find(|t| t.name == fk.ref_table) else {
                return Err(Error::new(format!(
                    "`{}` references `{}`, which no .sql file declares",
                    table.name, fk.ref_table
                )));
            };
            if fk.ref_columns.is_empty() && target.primary_key.is_empty() {
                return Err(Error::new(format!(
                    "`{}` references `{}`, which has no primary key",
                    table.name, fk.ref_table
                )));
            }
        }
    }
    let mut structs: Vec<(String, &str)> = Vec::new();
    for table in &schema.tables {
        let name = naming::struct_name(&table.name);
        if let Some((_, other)) = structs.iter().find(|(n, _)| *n == name) {
            return Err(Error::new(format!(
                "tables `{other}` and `{}` both become `{name}`; rename one",
                table.name
            )));
        }
        structs.push((name, &table.name));
    }
    for e in &schema.enums {
        let name = naming::pascal(naming::unqualified(&e.name));
        if structs.iter().any(|(n, _)| *n == name) {
            return Err(Error::new(format!(
                "enum `{}` and a table both become `{name}`; rename one",
                e.name
            )));
        }
    }
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for q in &parsed.queries {
        if seen.contains(&(q.module.as_str(), q.name.as_str())) {
            return Err(Error::at(
                &q.location,
                format!("a query named `{}` already exists in this file", q.name),
            ));
        }
        seen.push((&q.module, &q.name));
    }
    Ok(())
}

fn constraint_name(name: Option<&Ident>, default: impl FnOnce() -> String) -> String {
    name.map_or_else(default, ident)
}

/// An identifier as PostgreSQL stores it: unquoted names fold to lower case.
pub(crate) fn ident(i: &Ident) -> String {
    if i.quote_style.is_some() {
        i.value.clone()
    } else {
        i.value.to_ascii_lowercase()
    }
}

fn expr_ident(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(i) => ident(i),
        other => other.to_string(),
    }
}

/// `"app"."Users"` -> `app.Users`; unquoted parts fold to lower case.
pub(crate) fn object_name(name: &ObjectName) -> String {
    name.0
        .iter()
        .map(|part| part.as_ident().map_or_else(|| part.to_string(), ident))
        .collect::<Vec<_>>()
        .join(".")
}

/// Quote a possibly schema-qualified name for SQL.
pub(crate) fn quote(name: &str) -> String {
    name.split('.')
        .map(|part| format!("\"{}\"", part.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(text: &str) -> Result<Parsed, Error> {
        parse(&[SourceFile {
            path: "db/user.sql".into(),
            module: "user".into(),
            text: text.into(),
        }])
    }

    #[test]
    fn numeric_and_interval_spellings() {
        let p = parse_text(
            "CREATE TABLE plans (
                 price NUMERIC(10, 2) NOT NULL,
                 rate DECIMAL,
                 period INTERVAL NOT NULL,
                 grace INTERVAL DAY TO SECOND,
                 timeout INTERVAL(3),
                 tiers NUMERIC[]
             );",
        )
        .unwrap();
        let types: Vec<&str> = p.schema.tables[0]
            .columns
            .iter()
            .map(|c| c.sql_type.as_str())
            .collect();
        assert_eq!(
            types,
            [
                "NUMERIC(10,2)",
                "NUMERIC",
                "INTERVAL",
                "INTERVAL DAY TO SECOND",
                "INTERVAL(3)",
                "NUMERIC[]"
            ]
        );
    }

    #[test]
    fn tables_columns_and_constraints() {
        let p = parse_text(
            "CREATE TYPE mood AS ENUM ('happy', 'sad');
             -- A user.
             CREATE TABLE users (
                 id BIGSERIAL PRIMARY KEY,
                 -- Where we write.
                 email VARCHAR(255) NOT NULL UNIQUE,
                 manager_id BIGINT REFERENCES users (id) ON DELETE SET NULL,
                 mood mood NOT NULL DEFAULT 'happy',
                 -- rok:tenant
                 org_id INT NOT NULL,
                 lower_email TEXT GENERATED ALWAYS AS (lower(email)) STORED,
                 CHECK (org_id > 0)
             );
             CREATE INDEX users_org_idx ON users (org_id);
             -- name: find_by_email :one
             SELECT * FROM users WHERE email = $1;",
        )
        .unwrap();
        let t = &p.schema.tables[0];
        assert_eq!(t.doc, ["A user."]);
        assert_eq!(t.primary_key, ["id"]);
        let email = t.column("email").unwrap();
        assert_eq!(
            (email.sql_type.as_str(), email.nullable),
            ("VARCHAR(255)", false)
        );
        assert_eq!(email.doc, ["Where we write."]);
        assert_eq!(t.uniques[0].name, "users_email_key");
        assert_eq!(t.foreign_keys[0].name, "users_manager_id_fkey");
        assert_eq!(t.foreign_keys[0].on_delete.as_deref(), Some("SET NULL"));
        assert_eq!(
            t.column("mood").unwrap().default.as_deref(),
            Some("'happy'")
        );
        assert!(t.column("org_id").unwrap().tenant);
        assert_eq!(
            t.column("lower_email").unwrap().generated.as_deref(),
            Some("lower(email)")
        );
        assert_eq!(t.checks[0].name, "users_check");
        assert_eq!(p.schema.enums[0].values, ["happy", "sad"]);
        assert_eq!(p.schema.indexes[0].name, "users_org_idx");
        assert_eq!(p.queries[0].name, "find_by_email");
        assert_eq!(p.queries[0].kind, QueryKind::One);
    }

    #[test]
    fn extras_need_a_down() {
        let err = parse_text("CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;")
            .unwrap_err();
        assert!(err.to_string().contains("rok:down"), "{err}");
        let p = parse_text(
            "CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$ LANGUAGE sql;
             -- rok:down
             DROP FUNCTION f();",
        )
        .unwrap();
        assert_eq!(p.schema.extras[0].down, "DROP FUNCTION f()");
    }

    #[test]
    fn errors_point_at_the_file() {
        let err =
            parse_text("CREATE TABLE posts (id INT PRIMARY KEY, user_id INT REFERENCES users);")
                .unwrap_err();
        assert!(err.to_string().contains("users"), "{err}");
        let err = parse_text("\n-- name: x :maybe\nSELECT 1;").unwrap_err();
        assert!(err.to_string().contains("db/user.sql:3"), "{err}");
    }
}
