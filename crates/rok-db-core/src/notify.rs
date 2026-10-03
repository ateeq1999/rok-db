//! PostgreSQL `LISTEN`/`NOTIFY`, and typed change feeds for models.
//!
//! ```ignore
//! // Plain channels
//! let mut listener = db.listen(&["jobs"]).await?;
//! db.notify("jobs", "resize:42").await?;
//! let n = listener.recv().await?;              // Notification { channel: "jobs", payload: "resize:42", .. }
//!
//! // Model change feed (installs an AFTER trigger once)
//! User::install_change_notifications(&db).await?;
//! let mut changes = User::changes(&db).await?;
//! while let Ok(change) = changes.recv().await {
//!     println!("{:?} {}", change.op, change.key);
//!     let user: Option<User> = change.fetch(&db).await?;   // None after a delete
//! }
//! ```
//!
//! Notifications are delivered when the writing transaction commits, at most
//! once, and only to listeners connected at that time: use them to react
//! quickly (cache busting, websockets, waking workers), not as a durable log.

use std::fmt;
use std::marker::PhantomData;

use futures_core::stream::BoxStream;
use futures_util::StreamExt;
use sqlx::postgres::{PgListener, PgNotification};

use crate::{Db, Error, Executor, Expr, Model, Result};

/// A message received on a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The channel it was sent on.
    pub channel: String,
    /// The payload (empty if none).
    pub payload: String,
    /// Backend process ID of the sender.
    pub process_id: u32,
}

impl From<PgNotification> for Notification {
    fn from(n: PgNotification) -> Self {
        Self {
            channel: n.channel().to_owned(),
            payload: n.payload().to_owned(),
            process_id: n.process_id(),
        }
    }
}

/// A dedicated connection listening on one or more channels, created with
/// [`Db::listen`]. It reconnects automatically if the connection drops
/// (notifications sent meanwhile are lost).
pub struct Listener {
    inner: PgListener,
}

impl Listener {
    /// Wait for the next notification.
    pub async fn recv(&mut self) -> Result<Notification> {
        Ok(self.inner.recv().await?.into())
    }

    /// The next notification if one has already been received, without
    /// waiting.
    pub fn try_recv(&mut self) -> Option<Notification> {
        self.inner.next_buffered().map(Into::into)
    }

    /// Start listening on another channel.
    pub async fn listen(&mut self, channel: &str) -> Result<()> {
        Ok(self.inner.listen(channel).await?)
    }

    /// Stop listening on a channel.
    pub async fn unlisten(&mut self, channel: &str) -> Result<()> {
        Ok(self.inner.unlisten(channel).await?)
    }

    /// Turn into a stream of notifications.
    pub fn into_stream(self) -> BoxStream<'static, Result<Notification>> {
        self.inner
            .into_stream()
            .map(|n| n.map(Notification::from).map_err(Error::from))
            .boxed()
    }
}

impl fmt::Debug for Listener {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Listener")
    }
}

impl Db {
    /// Open a dedicated connection listening on `channels`.
    pub async fn listen(&self, channels: &[&str]) -> Result<Listener> {
        let mut inner = PgListener::connect_with(self.pool()).await?;
        inner.listen_all(channels.iter().copied()).await?;
        Ok(Listener { inner })
    }

    /// Send `payload` on `channel` (`SELECT pg_notify($1, $2)`). Inside a
    /// transaction use `rok_db::raw("SELECT pg_notify(?, ?)")` with `&mut *tx`
    /// so it's delivered on commit.
    pub async fn notify(&self, channel: &str, payload: &str) -> Result<()> {
        crate::raw("SELECT pg_notify(?, ?)")
            .bind(channel)
            .bind(payload)
            .execute(self)
            .await?;
        Ok(())
    }
}

/// The kind of change reported by a [`ChangeStream`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOp {
    /// A row was inserted.
    Insert,
    /// A row was updated.
    Update,
    /// A row was deleted.
    Delete,
}

/// One row change of model `M`.
pub struct Change<M> {
    /// What happened.
    pub op: ChangeOp,
    /// The primary key of the row, as text.
    pub key: String,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> Change<M> {
    /// Load the current row (`None` if it was deleted since). Default and
    /// soft-delete scopes are ignored.
    pub async fn fetch<'e, E: Executor<'e>>(&self, executor: E) -> Result<Option<M>> {
        let pk = quoted(M::PRIMARY_KEY);
        M::filter(Expr::raw(format!("{pk}::text = ?"), [self.key.as_str()]))
            .with_trashed()
            .unscoped()
            .on_primary()
            .first(executor)
            .await
    }

    fn parse(payload: &str) -> Result<Self> {
        let (op, key) = payload.split_once(':').ok_or_else(|| {
            Error::InvalidQuery(format!("malformed change notification {payload:?}"))
        })?;
        let op = match op {
            "INSERT" => ChangeOp::Insert,
            "UPDATE" => ChangeOp::Update,
            "DELETE" => ChangeOp::Delete,
            other => {
                return Err(Error::InvalidQuery(format!(
                    "unknown change operation {other:?}"
                )));
            }
        };
        Ok(Self {
            op,
            key: key.to_owned(),
            _model: PhantomData,
        })
    }
}

impl<M> Clone for Change<M> {
    fn clone(&self) -> Self {
        Self {
            op: self.op,
            key: self.key.clone(),
            _model: PhantomData,
        }
    }
}

impl<M> fmt::Debug for Change<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Change")
            .field("op", &self.op)
            .field("key", &self.key)
            .finish()
    }
}

impl<M> PartialEq for Change<M> {
    fn eq(&self, other: &Self) -> bool {
        self.op == other.op && self.key == other.key
    }
}

/// Row changes of model `M`, from [`Model::changes`].
pub struct ChangeStream<M> {
    listener: Listener,
    _model: PhantomData<fn() -> M>,
}

impl<M: Model> ChangeStream<M> {
    pub(crate) fn new(listener: Listener) -> Self {
        Self {
            listener,
            _model: PhantomData,
        }
    }

    /// Wait for the next change.
    pub async fn recv(&mut self) -> Result<Change<M>> {
        let n = self.listener.recv().await?;
        Change::parse(&n.payload)
    }

    /// Turn into a stream of changes.
    pub fn into_stream(self) -> BoxStream<'static, Result<Change<M>>> {
        self.listener
            .into_stream()
            .map(|n| n.and_then(|n| Change::parse(&n.payload)))
            .boxed()
    }
}

impl<M> fmt::Debug for ChangeStream<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ChangeStream")
    }
}

/// The channel a model's change trigger notifies.
pub(crate) fn channel<M: Model>() -> String {
    format!("rok:{}", M::TABLE)
}

fn quoted(ident: &str) -> String {
    let mut s = String::new();
    crate::sql::push_ident(&mut s, ident);
    s
}

fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub(crate) fn install_sql<M: Model>() -> Result<String> {
    let channel = channel::<M>();
    if channel.len() > 63 {
        return Err(Error::InvalidQuery(format!(
            "change channel {channel:?} exceeds PostgreSQL's 63-byte identifier limit"
        )));
    }
    let table = quoted(M::TABLE);
    Ok(format!(
        r#"CREATE OR REPLACE FUNCTION rok_db_notify_change() RETURNS trigger LANGUAGE plpgsql AS $rok$
DECLARE rec record;
BEGIN
    IF TG_OP = 'DELETE' THEN rec := OLD; ELSE rec := NEW; END IF;
    PERFORM pg_notify(TG_ARGV[0], TG_OP || ':' || coalesce(to_jsonb(rec) ->> TG_ARGV[1], ''));
    RETURN NULL;
END
$rok$;
DROP TRIGGER IF EXISTS rok_db_notify_change ON {table};
CREATE TRIGGER rok_db_notify_change AFTER INSERT OR UPDATE OR DELETE ON {table}
    FOR EACH ROW EXECUTE FUNCTION rok_db_notify_change({}, {});"#,
        literal(&channel),
        literal(M::PRIMARY_KEY),
    ))
}

pub(crate) fn uninstall_sql<M: Model>() -> String {
    format!(
        "DROP TRIGGER IF EXISTS rok_db_notify_change ON {}",
        quoted(M::TABLE)
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn literals_are_escaped() {
        assert_eq!(super::literal("it's"), "'it''s'");
    }
}
