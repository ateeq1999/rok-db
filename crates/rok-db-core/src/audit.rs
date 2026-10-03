//! A trigger-based audit log (feature `json`): every insert, update and
//! delete of an audited table — through rok-db, bulk queries or raw SQL —
//! is recorded in `rok_audit_log` with the old and new row as JSONB.
//!
//! ```ignore
//! rok_db::audit::install(&db).await?;                       // once: table + trigger function
//! rok_db::audit::enable::<User>(&db, &[User::PASSWORD_HASH]).await?;  // per table, minus secrets
//!
//! // Attribute changes to an actor: every transaction started inside
//! // `with_actor` records it (or call `tx.set_actor(..)` yourself).
//! rok_db::audit::with_actor("user:42", async {
//!     db.transaction(|tx| Box::pin(async move { user.save(&mut *tx).await })).await
//! }).await?;
//!
//! for entry in rok_db::audit::history::<User>(&db, user.id).await? {
//!     println!("{} {:?} by {:?}: {:?}", entry.op, entry.changed, entry.actor, entry.new_data);
//! }
//! ```
//!
//! Updates that change nothing are not recorded. Statements outside a
//! transaction have no actor unless the database role's `rok.actor` setting
//! is set.

use std::future::Future;
use std::time::{Duration, SystemTime};

use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};

use crate::{Column, Db, Executor, Expr, Hooks, Model, Result, Value};

/// The audit log table.
pub const TABLE: &str = "rok_audit_log";

tokio::task_local! {
    static ACTOR: String;
}

/// Run `future` with `actor` recorded on every transaction it begins via
/// [`Db::begin`] / [`Db::transaction`].
pub async fn with_actor<F: Future>(actor: impl Into<String>, future: F) -> F::Output {
    ACTOR.scope(actor.into(), future).await
}

/// The current actor, if inside [`with_actor`].
pub fn current_actor() -> Option<String> {
    ACTOR.try_with(String::clone).ok()
}

const INSTALL: &str = r#"
CREATE TABLE IF NOT EXISTS rok_audit_log (
    id BIGSERIAL PRIMARY KEY,
    table_name TEXT NOT NULL,
    record_key TEXT,
    op TEXT NOT NULL,
    actor TEXT,
    old_data JSONB,
    new_data JSONB,
    changed TEXT[] NOT NULL DEFAULT '{}',
    at_ms BIGINT NOT NULL DEFAULT (extract(epoch FROM clock_timestamp()) * 1000)::BIGINT
);
CREATE INDEX IF NOT EXISTS rok_audit_log_record ON rok_audit_log (table_name, record_key, id);
CREATE OR REPLACE FUNCTION rok_db_audit() RETURNS trigger LANGUAGE plpgsql AS $rok$
DECLARE
    old_j jsonb;
    new_j jsonb;
    excluded text[] := TG_ARGV[2:];
    changed text[] := '{}';
BEGIN
    IF TG_OP <> 'INSERT' THEN old_j := to_jsonb(OLD) - excluded; END IF;
    IF TG_OP <> 'DELETE' THEN new_j := to_jsonb(NEW) - excluded; END IF;
    IF TG_OP = 'UPDATE' THEN
        SELECT coalesce(array_agg(k ORDER BY k), '{}') INTO changed
        FROM jsonb_object_keys(new_j) AS k
        WHERE new_j -> k IS DISTINCT FROM old_j -> k;
        IF cardinality(changed) = 0 THEN RETURN NULL; END IF;
    END IF;
    INSERT INTO rok_audit_log (table_name, record_key, op, actor, old_data, new_data, changed)
    VALUES (
        TG_ARGV[1],
        coalesce(new_j, old_j) ->> TG_ARGV[0],
        TG_OP,
        nullif(current_setting('rok.actor', true), ''),
        old_j,
        new_j,
        changed
    );
    RETURN NULL;
END
$rok$;
"#;

fn quoted(ident: &str) -> String {
    let mut s = String::new();
    crate::sql::push_ident(&mut s, ident);
    s
}

fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Create the `rok_audit_log` table and the trigger function (idempotent).
pub async fn install(db: &Db) -> Result<()> {
    db.execute(INSTALL).await?;
    Ok(())
}

/// Start auditing `M`'s table, leaving the `exclude`d columns (secrets,
/// large blobs) out of the recorded data. Re-running replaces the trigger.
pub async fn enable<M: Model>(db: &Db, exclude: &[Column<M>]) -> Result<()> {
    let mut args = vec![literal(M::PRIMARY_KEY), literal(M::TABLE)];
    args.extend(exclude.iter().map(|c| literal(c.name())));
    let table = quoted(M::TABLE);
    db.execute(&format!(
        "DROP TRIGGER IF EXISTS rok_db_audit ON {table};
         CREATE TRIGGER rok_db_audit AFTER INSERT OR UPDATE OR DELETE ON {table}
             FOR EACH ROW EXECUTE FUNCTION rok_db_audit({});",
        args.join(", ")
    ))
    .await?;
    Ok(())
}

/// Stop auditing `M`'s table (recorded entries are kept).
pub async fn disable<M: Model>(db: &Db) -> Result<()> {
    db.execute(&format!(
        "DROP TRIGGER IF EXISTS rok_db_audit ON {}",
        quoted(M::TABLE)
    ))
    .await?;
    Ok(())
}

/// Every audit entry of one `M` record, oldest first.
pub async fn history<'e, M: Model>(
    executor: impl Executor<'e>,
    key: impl Into<Value>,
) -> Result<Vec<AuditEntry>> {
    AuditEntry::for_table::<M>()
        .filter(Expr::raw(
            format!("{} = ?::text", quoted("record_key")),
            [key.into()],
        ))
        .order_by(AuditEntry::ID)
        .all(executor)
        .await
}

/// One recorded change. Query the log like any model:
/// `AuditEntry::for_table::<User>().filter(AuditEntry::ACTOR.eq("user:42"))`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    /// Sequential id.
    pub id: i64,
    /// The audited table (as named by the model).
    pub table_name: String,
    /// The record's primary key, as text.
    pub record_key: Option<String>,
    /// `INSERT`, `UPDATE` or `DELETE`.
    pub op: String,
    /// Who made the change, if known.
    pub actor: Option<String>,
    /// The row before the change (`None` for inserts).
    pub old_data: Option<serde_json::Value>,
    /// The row after the change (`None` for deletes).
    pub new_data: Option<serde_json::Value>,
    /// Columns that changed (updates only).
    pub changed: Vec<String>,
    /// When the change happened, in milliseconds since the Unix epoch.
    pub at_ms: i64,
}

impl AuditEntry {
    /// The `id` column.
    pub const ID: Column<Self> = Column::new("id");
    /// The `table_name` column.
    pub const TABLE_NAME: Column<Self> = Column::new("table_name");
    /// The `record_key` column.
    pub const RECORD_KEY: Column<Self> = Column::new("record_key");
    /// The `op` column.
    pub const OP: Column<Self> = Column::new("op");
    /// The `actor` column.
    pub const ACTOR: Column<Self> = Column::new("actor");
    /// The `at_ms` column.
    pub const AT_MS: Column<Self> = Column::new("at_ms");

    /// Entries of `M`'s table.
    pub fn for_table<M: Model>() -> crate::Select<Self> {
        Self::filter(Self::TABLE_NAME.eq(M::TABLE))
    }

    /// When the change happened.
    pub fn at(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(self.at_ms.max(0) as u64)
    }
}

impl<'r> FromRow<'r, PgRow> for AuditEntry {
    fn from_row(row: &'r PgRow) -> std::result::Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            table_name: row.try_get("table_name")?,
            record_key: row.try_get("record_key")?,
            op: row.try_get("op")?,
            actor: row.try_get("actor")?,
            old_data: row.try_get("old_data")?,
            new_data: row.try_get("new_data")?,
            changed: row.try_get("changed")?,
            at_ms: row.try_get("at_ms")?,
        })
    }
}

impl Hooks for AuditEntry {}

impl Model for AuditEntry {
    const TABLE: &'static str = TABLE;
    const PRIMARY_KEY: &'static str = "id";
    const COLUMNS: &'static [&'static str] = &[
        "id",
        "table_name",
        "record_key",
        "op",
        "actor",
        "old_data",
        "new_data",
        "changed",
        "at_ms",
    ];
    const GENERATED: &'static [&'static str] = &["id", "at_ms"];

    fn primary_key(&self) -> Value {
        Value::from(self.id)
    }

    fn values(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("id", Value::from(self.id)),
            ("table_name", Value::from(&self.table_name)),
            ("record_key", Value::from(&self.record_key)),
            ("op", Value::from(&self.op)),
            ("actor", Value::from(&self.actor)),
            ("old_data", Value::from(&self.old_data)),
            ("new_data", Value::from(&self.new_data)),
            ("changed", Value::from(&self.changed)),
            ("at_ms", Value::from(self.at_ms)),
        ]
    }
}
