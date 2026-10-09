//! The schema model built from `.sql` files. It is also the snapshot
//! (`schema.json`) that the next migration is computed against.

use serde::{Deserialize, Serialize};

/// Everything the `.sql` files declare, in a stable order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    /// `CREATE TYPE ... AS ENUM`.
    pub enums: Vec<EnumType>,
    /// `CREATE TABLE`, in file order.
    pub tables: Vec<Table>,
    /// `CREATE INDEX`.
    pub indexes: Vec<Index>,
    /// Other statements (extensions, views, functions, comments ...).
    pub extras: Vec<Extra>,
}

/// A PostgreSQL enum type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumType {
    /// Type name, possibly schema-qualified.
    pub name: String,
    /// Labels, in order.
    pub values: Vec<String>,
    /// Module (file stem) that declares it.
    pub module: String,
    /// Doc comment lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doc: Vec<String>,
}

/// A table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    /// Table name, possibly schema-qualified.
    pub name: String,
    /// Module (file stem) that declares it.
    pub module: String,
    /// Doc comment lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doc: Vec<String>,
    /// Columns, in order.
    pub columns: Vec<Column>,
    /// Primary key columns (empty: none).
    pub primary_key: Vec<String>,
    /// `UNIQUE` constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uniques: Vec<Unique>,
    /// `FOREIGN KEY` constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub foreign_keys: Vec<ForeignKey>,
    /// `CHECK` constraints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<Check>,
}

impl Table {
    /// The column named `name`.
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// Whether `columns` are covered by a single-column or composite unique
    /// constraint (or are the primary key).
    pub fn is_unique(&self, columns: &[String]) -> bool {
        self.primary_key == columns || self.uniques.iter().any(|u| u.columns == columns)
    }
}

/// A column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    /// Column name.
    pub name: String,
    /// Canonical SQL type (`BIGINT`, `VARCHAR(255)`, `user_role`).
    pub sql_type: String,
    /// Whether `NULL` is allowed.
    pub nullable: bool,
    /// `DEFAULT` expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// `GENERATED ALWAYS AS (expr) STORED` expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<String>,
    /// `GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// Marked `-- rok:tenant`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tenant: bool,
    /// Doc comment lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub doc: Vec<String>,
}

impl Column {
    /// Whether the database fills the column in when it is not written.
    pub fn has_database_value(&self) -> bool {
        self.default.is_some()
            || self.generated.is_some()
            || self.identity.is_some()
            || crate::types::is_serial(&self.sql_type)
    }
}

/// A `UNIQUE` constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unique {
    /// Constraint name.
    pub name: String,
    /// Columns.
    pub columns: Vec<String>,
}

/// A `FOREIGN KEY` constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    /// Constraint name.
    pub name: String,
    /// Referencing columns.
    pub columns: Vec<String>,
    /// Referenced table.
    pub ref_table: String,
    /// Referenced columns.
    pub ref_columns: Vec<String>,
    /// `ON DELETE` action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_delete: Option<String>,
    /// `ON UPDATE` action.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_update: Option<String>,
}

/// A `CHECK` constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Constraint name.
    pub name: String,
    /// Boolean expression.
    pub expr: String,
}

/// A `CREATE INDEX` statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    /// Index name.
    pub name: String,
    /// Indexed table.
    pub table: String,
    /// The statement, as written (normalized).
    pub sql: String,
}

/// Any other statement, applied as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extra {
    /// SQL that creates it.
    pub up: String,
    /// SQL that removes it (empty: can't be undone automatically).
    pub down: String,
    /// Runs before tables (extensions) rather than after them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub before_tables: bool,
}

/// What a query returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    /// `:one`: `Option<T>`.
    One,
    /// `:one!`: `T`, an error when there is no row.
    OneRequired,
    /// `:many`: `Vec<T>`.
    Many,
    /// `:stream`: a stream of `T`.
    Stream,
    /// `:exec`: rows affected.
    Exec,
}

impl QueryKind {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            ":one" => Self::One,
            ":one!" => Self::OneRequired,
            ":many" => Self::Many,
            ":stream" => Self::Stream,
            ":exec" => Self::Exec,
            _ => return None,
        })
    }
}

/// An annotated query (`-- name: find_by_email :one`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// Function name.
    pub name: String,
    /// Module (file stem) it belongs to.
    pub module: String,
    /// What it returns.
    pub kind: QueryKind,
    /// The SQL, as written.
    pub sql: String,
    /// Doc comment lines.
    pub doc: Vec<String>,
    /// `-- param: $2 limit` overrides: (position, name).
    pub param_names: Vec<(usize, String)>,
    /// Where it is, for messages (`db/user.sql:12`).
    pub location: String,
}
