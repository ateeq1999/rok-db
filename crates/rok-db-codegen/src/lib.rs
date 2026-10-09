//! Generate a rok-db crate from `.sql` files: models and enums from
//! `CREATE TABLE` / `CREATE TYPE`, typed functions from sqlc-style annotated
//! queries, and migrations computed from schema changes.
//!
//! Most people use the `rok-db-gen` binary; see the crate README.

mod ddl;
mod emit;
mod ir;
mod naming;
mod parse;
mod split;
mod types;

pub use ddl::Rename;
pub use ir::{Column, EnumType, Extra, ForeignKey, Index, Query, QueryKind, Schema, Table};
pub use parse::{Parsed, SourceFile, parse};

use std::fmt;

/// A generation error, with the file and line when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    message: String,
}

impl Error {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn at(location: &str, message: impl fmt::Display) -> Self {
        Self::new(format!("{location}: {message}"))
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
