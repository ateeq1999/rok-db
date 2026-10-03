//! Bulk loading with PostgreSQL `COPY … FROM STDIN (FORMAT binary)`.

use sqlx::postgres::PgConnection;

use crate::{Db, Error, Model, Result, Tx};

/// Where [`Model::copy_in`] runs: a pool (one connection is borrowed), a
/// transaction or a plain connection.
#[derive(Debug)]
pub enum CopyTarget<'a> {
    /// A connection borrowed from the pool.
    Db(&'a Db),
    /// An open transaction.
    Tx(&'a mut Tx),
    /// A plain connection.
    Connection(&'a mut PgConnection),
}

impl<'a> From<&'a Db> for CopyTarget<'a> {
    fn from(db: &'a Db) -> Self {
        CopyTarget::Db(db)
    }
}

impl<'a> From<&'a mut Tx> for CopyTarget<'a> {
    fn from(tx: &'a mut Tx) -> Self {
        CopyTarget::Tx(tx)
    }
}

impl<'a> From<&'a mut PgConnection> for CopyTarget<'a> {
    fn from(conn: &'a mut PgConnection) -> Self {
        CopyTarget::Connection(conn)
    }
}

const SIGNATURE: &[u8] = b"PGCOPY\n\xff\r\n\0";
/// Flush to the server roughly every megabyte.
const CHUNK: usize = 1 << 20;

pub(crate) async fn copy_in<M: Model>(target: CopyTarget<'_>, records: &[M]) -> Result<u64> {
    if records.is_empty() {
        return Ok(0);
    }
    for record in records {
        crate::model::pre_insert(record)?;
    }
    let columns: Vec<&'static str> = M::COLUMNS
        .iter()
        .copied()
        .filter(|c| !M::GENERATED.contains(c))
        .collect();
    let mut statement = String::from("COPY ");
    crate::sql::push_ident(&mut statement, M::TABLE);
    statement.push_str(" (");
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            statement.push_str(", ");
        }
        crate::sql::push_ident(&mut statement, c);
    }
    statement.push_str(") FROM STDIN (FORMAT binary)");

    match target {
        CopyTarget::Db(db) => {
            let mut conn = db.pool().acquire().await?;
            let rows = run(&mut conn, &statement, &columns, records).await?;
            if let Some(cache) = db.cache() {
                cache.invalidate(M::TABLE);
            }
            Ok(rows)
        }
        CopyTarget::Tx(tx) => {
            let rows = run(tx, &statement, &columns, records).await?;
            tx.note_write(M::TABLE);
            Ok(rows)
        }
        CopyTarget::Connection(conn) => run(conn, &statement, &columns, records).await,
    }
}

async fn run<M: Model>(
    conn: &mut PgConnection,
    statement: &str,
    columns: &[&'static str],
    records: &[M],
) -> Result<u64> {
    let start = std::time::Instant::now();
    let mut copy = conn.copy_in_raw(statement).await?;
    let mut buf = Vec::with_capacity(CHUNK + 4096);
    buf.extend_from_slice(SIGNATURE);
    buf.extend_from_slice(&0_i32.to_be_bytes()); // flags
    buf.extend_from_slice(&0_i32.to_be_bytes()); // header extension length

    let field_count = i16::try_from(columns.len())
        .map_err(|_| Error::InvalidQuery("too many columns for COPY".into()))?;
    for record in records {
        buf.extend_from_slice(&field_count.to_be_bytes());
        let values = record.values();
        for (column, value) in values.iter().filter(|(c, _)| columns.contains(c)) {
            let tenant = crate::model::tenant_override::<M>(column);
            let value = tenant.as_ref().unwrap_or(value);
            match value.encode_binary() {
                Ok(Some(bytes)) => {
                    let len = i32::try_from(bytes.len()).map_err(|_| {
                        Error::InvalidQuery(format!("value of `{column}` is too large for COPY"))
                    })?;
                    buf.extend_from_slice(&len.to_be_bytes());
                    buf.extend_from_slice(&bytes);
                }
                Ok(None) => buf.extend_from_slice(&(-1_i32).to_be_bytes()),
                Err(e) => {
                    let _ = copy.abort("rok-db: failed to encode a value").await;
                    return Err(e);
                }
            }
        }
        if buf.len() >= CHUNK {
            copy.send(std::mem::take(&mut buf)).await?;
            buf.reserve(CHUNK + 4096);
        }
    }
    buf.extend_from_slice(&(-1_i16).to_be_bytes()); // trailer
    copy.send(buf).await?;
    let rows = copy.finish().await?;
    crate::metrics::query(statement, start.elapsed(), Some(rows), false);
    tracing::debug!(
        target: "rok_db::query",
        sql = statement,
        rows,
        elapsed_ms = start.elapsed().as_secs_f64() * 1000.0,
        "copy"
    );
    Ok(rows)
}
