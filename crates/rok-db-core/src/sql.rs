use std::fmt::{self, Write};

use sqlx::postgres::PgArguments;

use crate::{Result, Value};

/// A rendered SQL statement together with its bound parameters.
///
/// Every builder exposes a `to_sql()` method returning this type, which is
/// handy for logging, debugging and testing generated queries.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sql {
    sql: String,
    params: Vec<Value>,
}

impl Sql {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The SQL text, using PostgreSQL `$n` placeholders.
    pub fn as_str(&self) -> &str {
        &self.sql
    }

    /// The parameters bound to the placeholders, in order.
    pub fn params(&self) -> &[Value] {
        &self.params
    }

    /// Split into the SQL text and its parameters.
    pub fn into_parts(self) -> (String, Vec<Value>) {
        (self.sql, self.params)
    }

    pub(crate) fn push(&mut self, s: &str) -> &mut Self {
        self.sql.push_str(s);
        self
    }

    pub(crate) fn push_ident(&mut self, ident: &str) -> &mut Self {
        push_ident(&mut self.sql, ident);
        self
    }

    pub(crate) fn bind(&mut self, value: Value) -> &mut Self {
        self.params.push(value);
        let _ = write!(self.sql, "${}", self.params.len());
        self
    }

    /// Append raw SQL in which every `?` is replaced by the next value.
    pub(crate) fn push_raw(&mut self, raw: &str, params: &[Value]) -> &mut Self {
        let mut params = params.iter();
        for (i, part) in raw.split('?').enumerate() {
            if i > 0 {
                match params.next() {
                    Some(v) => self.bind(v.clone()),
                    None => self.push("?"),
                };
            }
            self.push(part);
        }
        self
    }

    /// Append raw SQL using either `?` placeholders (numbered here) or native
    /// `$n` placeholders (parameters are appended as-is).
    pub(crate) fn push_raw_params(&mut self, raw: &str, params: &[Value]) -> &mut Self {
        if raw.contains('?') {
            self.push_raw(raw, params)
        } else {
            self.push(raw);
            self.params.extend_from_slice(params);
            self
        }
    }

    pub(crate) fn push_list<T>(
        &mut self,
        items: impl IntoIterator<Item = T>,
        sep: &str,
        mut f: impl FnMut(&mut Self, T),
    ) -> &mut Self {
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                self.push(sep);
            }
            f(self, item);
        }
        self
    }

    pub(crate) fn arguments(&self) -> Result<PgArguments> {
        let mut args = PgArguments::default();
        for value in &self.params {
            value.clone().bind(&mut args)?;
        }
        Ok(args)
    }
}

impl fmt::Display for Sql {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.sql)
    }
}

/// Quote an identifier, splitting `schema.table` into its parts.
pub(crate) fn push_ident(out: &mut String, ident: &str) {
    for (i, part) in ident.split('.').enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push('"');
        out.push_str(&part.replace('"', "\"\""));
        out.push('"');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_identifiers() {
        let mut s = String::new();
        push_ident(&mut s, "public.us\"ers");
        assert_eq!(s, r#""public"."us""ers""#);
    }

    #[test]
    fn raw_placeholders_are_numbered() {
        let mut sql = Sql::new();
        sql.bind(1.into()).push(" AND ");
        sql.push_raw("a = ? OR b = ?", &[2.into(), 3.into()]);
        assert_eq!(sql.as_str(), "$1 AND a = $2 OR b = $3");
        assert_eq!(sql.params().len(), 3);
    }
}
