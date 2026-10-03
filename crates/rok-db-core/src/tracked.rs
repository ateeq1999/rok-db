use std::fmt;
use std::ops::{Deref, DerefMut};

use crate::{Column, Executor, Model, Result, Value};

/// A record that remembers its loaded values, so [`save`](Tracked::save)
/// writes only the columns you changed — or nothing at all.
///
/// ```ignore
/// let mut user = User::find_or_fail(&db, 1).await?.track();
/// user.name = "Ann".into();               // DerefMut to the record
/// assert!(user.is_dirty());
/// user.save(&db).await?;                  // UPDATE users SET name = $1 … WHERE id = $2
/// user.save(&db).await?;                  // no changes: no query
/// let user: User = user.into_inner();
/// ```
///
/// Writing only changed columns avoids overwriting concurrent changes to
/// *other* columns and keeps statements small. Validation and the
/// `before_save`/`after_save` hooks still run on every write.
pub struct Tracked<M> {
    record: M,
    original: Vec<(&'static str, Value)>,
}

impl<M: Model> Tracked<M> {
    /// Start tracking `record` from its current values.
    pub fn new(record: M) -> Self {
        let original = record.values();
        Self { record, original }
    }

    /// The columns whose values differ from the snapshot, with the old and
    /// new values.
    pub fn changes(&self) -> Vec<(&'static str, Value, Value)> {
        self.record
            .values()
            .into_iter()
            .zip(&self.original)
            .filter(|((_, new), (_, old))| new != old)
            .map(|((column, new), (_, old))| (column, old.clone(), new))
            .collect()
    }

    /// `true` if any column changed since loading (or the last save).
    pub fn is_dirty(&self) -> bool {
        self.record
            .values()
            .iter()
            .zip(&self.original)
            .any(|((_, new), (_, old))| new != old)
    }

    /// `true` if `column` changed.
    pub fn is_changed(&self, column: Column<M>) -> bool {
        self.changes().iter().any(|(c, _, _)| *c == column.name())
    }

    /// Write the changed columns (if any) and refresh the record and its
    /// snapshot from the stored row. Returns `true` if a write happened.
    pub async fn save<'e, E: Executor<'e>>(&mut self, executor: E) -> Result<bool> {
        let changed: Vec<Column<M>> = self
            .changes()
            .into_iter()
            .map(|(c, _, _)| Column::new(c))
            .collect();
        if changed.is_empty() {
            return Ok(false);
        }
        let stored = self.record.save_only(executor, Some(changed)).await?;
        *self = Tracked::new(stored);
        Ok(true)
    }

    /// Accept the current values as the new baseline without writing them.
    pub fn mark_clean(&mut self) {
        self.original = self.record.values();
    }

    /// Stop tracking and return the record.
    pub fn into_inner(self) -> M {
        self.record
    }
}

impl<M> Deref for Tracked<M> {
    type Target = M;

    fn deref(&self) -> &M {
        &self.record
    }
}

impl<M> DerefMut for Tracked<M> {
    fn deref_mut(&mut self) -> &mut M {
        &mut self.record
    }
}

impl<M: fmt::Debug> fmt::Debug for Tracked<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Tracked").field(&self.record).finish()
    }
}
