use std::fmt;

/// Convenient alias for results returned by rok-db.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Every error rok-db can produce.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A query that must return a row returned nothing.
    #[error("no `{table}` record found{}", DisplayKey(.key))]
    NotFound {
        /// Table that was queried.
        table: &'static str,
        /// Primary key that was looked up, when known.
        key: Option<String>,
    },

    /// An optimistic-locking check failed: the record was changed by someone
    /// else since it was loaded (its `#[rok(version)]` column no longer
    /// matches). Reload the record and retry.
    #[error("`{table}` record with primary key {key} was modified concurrently")]
    Conflict {
        /// Table that was written.
        table: &'static str,
        /// Primary key of the record.
        key: String,
    },

    /// The record failed validation (`#[rok(validate(…))]`); nothing was
    /// written.
    #[error("validation failed: {0}")]
    Validation(crate::validate::ValidationErrors),

    /// A lifecycle hook rejected the operation or failed.
    #[error("{0}")]
    Hook(#[source] sqlx::error::BoxDynError),

    /// A pagination cursor could not be decoded.
    #[error("invalid cursor: {0}")]
    InvalidCursor(String),

    /// Binding a parameter to the query failed.
    #[error("failed to encode query parameter: {0}")]
    Encode(#[source] sqlx::error::BoxDynError),

    /// The configuration (e.g. `DATABASE_URL`) is missing or invalid.
    #[error("configuration error: {0}")]
    Config(String),

    /// A query was built in a way that cannot be executed.
    #[error("invalid query: {0}")]
    InvalidQuery(String),

    /// An error reported by the database driver.
    #[error(transparent)]
    Database(#[from] sqlx::Error),

    /// An error raised while running migrations.
    #[cfg(feature = "migrate")]
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

impl Error {
    /// `true` if this is a [`Error::NotFound`] (or the driver's own `RowNotFound`).
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Error::NotFound { .. } | Error::Database(sqlx::Error::RowNotFound)
        )
    }

    /// Wrap any error returned from a [`Hooks`](crate::Hooks) method.
    pub fn hook(error: impl Into<sqlx::error::BoxDynError>) -> Self {
        Error::Hook(error.into())
    }

    /// The validation errors, if this is an [`Error::Validation`].
    pub fn validation_errors(&self) -> Option<&crate::validate::ValidationErrors> {
        match self {
            Error::Validation(errors) => Some(errors),
            _ => None,
        }
    }

    /// `true` if this is an optimistic-locking [`Error::Conflict`].
    pub fn is_conflict(&self) -> bool {
        matches!(self, Error::Conflict { .. })
    }

    /// `true` for a serialization failure or deadlock (SQLSTATE `40001` /
    /// `40P01`): the transaction can be retried as a whole.
    pub fn is_serialization_failure(&self) -> bool {
        self.db_error()
            .and_then(|e| e.code())
            .is_some_and(|code| code == "40001" || code == "40P01")
    }

    /// `true` if the database rejected the query because of a unique constraint.
    pub fn is_unique_violation(&self) -> bool {
        self.db_error().is_some_and(|e| e.is_unique_violation())
    }

    /// `true` if the database rejected the query because of a foreign key constraint.
    pub fn is_foreign_key_violation(&self) -> bool {
        self.db_error()
            .is_some_and(|e| e.is_foreign_key_violation())
    }

    /// The name of the violated constraint, if any.
    pub fn constraint(&self) -> Option<&str> {
        self.db_error().and_then(|e| e.constraint())
    }

    fn db_error(&self) -> Option<&dyn sqlx::error::DatabaseError> {
        match self {
            Error::Database(sqlx::Error::Database(e)) => Some(e.as_ref()),
            _ => None,
        }
    }

    pub(crate) fn not_found<M: crate::Model>(key: Option<String>) -> Self {
        Error::NotFound {
            table: M::TABLE,
            key,
        }
    }
}

struct DisplayKey<'a>(&'a Option<String>);

impl fmt::Display for DisplayKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(key) => write!(f, " with primary key {key}"),
            None => Ok(()),
        }
    }
}
