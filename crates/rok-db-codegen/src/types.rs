//! SQL types: canonical spelling for migrations and diffs, and Rust types.

use std::collections::BTreeSet;

/// A Cargo feature of rok-db a generated type needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Feature {
    Chrono,
    Uuid,
    Json,
    Decimal,
}

impl Feature {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Feature::Chrono => "chrono",
            Feature::Uuid => "uuid",
            Feature::Json => "json",
            Feature::Decimal => "decimal",
        }
    }
}

/// Canonical spelling of a column type, so `int4`, `INT` and `integer`
/// compare equal. Unknown types (enums, domains) keep their name, lower case.
pub(crate) fn canonical(sql_type: &str) -> String {
    let trimmed = sql_type.trim();
    let (base, dims) = strip_array(trimmed);
    let (name, args) = match base.find('(') {
        Some(open) => (base[..open].trim(), Some(base[open..].replace(' ', ""))),
        None => (base, None),
    };
    let lower = name.to_ascii_lowercase();
    let lower = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    let canonical = match lower.as_str() {
        "int2" | "smallint" => "SMALLINT".to_owned(),
        "int" | "int4" | "integer" => "INTEGER".to_owned(),
        "int8" | "bigint" => "BIGINT".to_owned(),
        "serial2" | "smallserial" => "SMALLSERIAL".to_owned(),
        "serial" | "serial4" => "SERIAL".to_owned(),
        "serial8" | "bigserial" => "BIGSERIAL".to_owned(),
        "real" | "float4" => "REAL".to_owned(),
        "double precision" | "float8" | "double" => "DOUBLE PRECISION".to_owned(),
        "float" if args.is_none() => "DOUBLE PRECISION".to_owned(),
        "bool" | "boolean" => "BOOLEAN".to_owned(),
        "text" => "TEXT".to_owned(),
        "varchar" | "character varying" => with_args("VARCHAR", args.as_deref()),
        "char" | "character" | "bpchar" => with_args("CHAR", args.as_deref()),
        "name" => "NAME".to_owned(),
        "bytea" => "BYTEA".to_owned(),
        "uuid" => "UUID".to_owned(),
        "timestamptz" | "timestamp with time zone" => "TIMESTAMPTZ".to_owned(),
        "timestamp" | "timestamp without time zone" => "TIMESTAMP".to_owned(),
        "date" => "DATE".to_owned(),
        "time" | "time without time zone" => "TIME".to_owned(),
        "json" => "JSON".to_owned(),
        "jsonb" => "JSONB".to_owned(),
        "numeric" | "decimal" => with_args("NUMERIC", args.as_deref()),
        // `INTERVAL`, `INTERVAL(3)`, `INTERVAL DAY TO SECOND`: the fields are kept so a
        // change shows up in a diff.
        interval if interval == "interval" || interval.starts_with("interval ") => {
            with_args(&interval.to_ascii_uppercase(), args.as_deref())
        }
        other => match args {
            Some(args) => format!("{other}{args}"),
            None => other.to_owned(),
        },
    };
    format!("{canonical}{}", "[]".repeat(dims))
}

fn with_args(name: &str, args: Option<&str>) -> String {
    format!("{name}{}", args.unwrap_or(""))
}

/// Split `TEXT[][]` / `TEXT ARRAY` into the element type and dimensions.
fn strip_array(sql_type: &str) -> (&str, usize) {
    let mut base = sql_type.trim();
    let mut dims = 0;
    loop {
        if let Some(rest) = base.strip_suffix("[]") {
            base = rest.trim_end();
            dims += 1;
        } else if base.len() > 6 && base[base.len() - 6..].eq_ignore_ascii_case(" array") {
            base = base[..base.len() - 6].trim_end();
            dims += 1;
        } else {
            return (base, dims);
        }
    }
}

/// Whether a canonical type is filled in by the database (`SERIAL` ...).
pub(crate) fn is_serial(canonical: &str) -> bool {
    matches!(canonical, "SMALLSERIAL" | "SERIAL" | "BIGSERIAL")
}

/// The Rust type for a canonical SQL type. `enums` maps enum type names to
/// their Rust paths. Records the rok-db features the type needs.
pub(crate) fn rust_type(
    canonical: &str,
    enum_path: &dyn Fn(&str) -> Option<String>,
    features: &mut BTreeSet<Feature>,
) -> Result<String, String> {
    let (base, dims) = strip_array(canonical);
    if dims > 1 {
        return Err(format!(
            "multi-dimensional arrays (`{canonical}`) are not supported"
        ));
    }
    let name = base.split('(').next().unwrap_or(base);
    let name = if name == "INTERVAL" || name.starts_with("INTERVAL ") {
        "INTERVAL"
    } else {
        name
    };
    let scalar = match name {
        "SMALLINT" | "SMALLSERIAL" => "i16".to_owned(),
        "INTEGER" | "SERIAL" => "i32".to_owned(),
        "BIGINT" | "BIGSERIAL" => "i64".to_owned(),
        "REAL" => "f32".to_owned(),
        "DOUBLE PRECISION" => "f64".to_owned(),
        "BOOLEAN" => "bool".to_owned(),
        "TEXT" | "VARCHAR" | "CHAR" | "NAME" => "String".to_owned(),
        "BYTEA" if dims == 0 => "Vec<u8>".to_owned(),
        "UUID" => {
            features.insert(Feature::Uuid);
            "rok_db::sqlx::types::Uuid".to_owned()
        }
        "TIMESTAMPTZ" => {
            features.insert(Feature::Chrono);
            "rok_db::sqlx::types::chrono::DateTime<rok_db::sqlx::types::chrono::Utc>".to_owned()
        }
        "TIMESTAMP" if dims == 0 => {
            features.insert(Feature::Chrono);
            "rok_db::sqlx::types::chrono::NaiveDateTime".to_owned()
        }
        "DATE" => {
            features.insert(Feature::Chrono);
            "rok_db::sqlx::types::chrono::NaiveDate".to_owned()
        }
        "TIME" if dims == 0 => {
            features.insert(Feature::Chrono);
            "rok_db::sqlx::types::chrono::NaiveTime".to_owned()
        }
        "JSON" | "JSONB" if dims == 0 => {
            features.insert(Feature::Json);
            "rok_db::sqlx::types::JsonValue".to_owned()
        }
        "NUMERIC" => {
            features.insert(Feature::Decimal);
            "rok_db::sqlx::types::Decimal".to_owned()
        }
        "INTERVAL" => "rok_db::sqlx::postgres::types::PgInterval".to_owned(),
        other => match enum_path(other) {
            Some(path) if dims == 0 => path,
            Some(_) => {
                return Err(format!(
                    "arrays of enum types (`{canonical}`) are not supported"
                ));
            }
            None => {
                return Err(format!(
                    "`{canonical}` has no rok-db column type; use a built-in type or an enum \
                     declared in the .sql files"
                ));
            }
        },
    };
    if dims == 1 {
        let supported = [
            "String",
            "bool",
            "i16",
            "i32",
            "i64",
            "f32",
            "f64",
            "rok_db::sqlx::types::Uuid",
            "rok_db::sqlx::types::chrono::DateTime<rok_db::sqlx::types::chrono::Utc>",
            "rok_db::sqlx::types::chrono::NaiveDate",
            "rok_db::sqlx::types::Decimal",
            "rok_db::sqlx::postgres::types::PgInterval",
        ];
        if !supported.contains(&scalar.as_str()) {
            return Err(format!("arrays of `{base}` are not supported"));
        }
        return Ok(format!("Vec<{scalar}>"));
    }
    Ok(scalar)
}

/// The canonical SQL type for a PostgreSQL type name as reported by sqlx
/// (`INT8`, `TEXT[]`, `user_role`), for query parameters and columns.
pub(crate) fn canonical_from_pg(name: &str) -> String {
    match name {
        "BPCHAR" => "CHAR".to_owned(),
        "BPCHAR[]" => "CHAR[]".to_owned(),
        other => canonical(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_spellings() {
        assert_eq!(canonical("int4"), "INTEGER");
        assert_eq!(canonical("character varying (255)"), "VARCHAR(255)");
        assert_eq!(canonical("timestamp with time zone"), "TIMESTAMPTZ");
        assert_eq!(canonical("text[]"), "TEXT[]");
        assert_eq!(canonical("INT ARRAY"), "INTEGER[]");
        assert_eq!(canonical("User_Role"), "user_role");
        assert_eq!(canonical("numeric(10, 2)"), "NUMERIC(10,2)");
        assert_eq!(canonical("decimal"), "NUMERIC");
        assert_eq!(canonical("interval"), "INTERVAL");
        assert_eq!(
            canonical("Interval Day To Second"),
            "INTERVAL DAY TO SECOND"
        );
        assert_eq!(canonical("interval(3)"), "INTERVAL(3)");
        assert_eq!(canonical("interval[]"), "INTERVAL[]");
    }

    #[test]
    fn rust_types() {
        let mut features = BTreeSet::new();
        let enums = |name: &str| (name == "mood").then(|| "crate::user::Mood".to_owned());
        let t = |s: &str, f: &mut BTreeSet<Feature>| rust_type(&canonical(s), &enums, f);
        assert_eq!(t("bigserial", &mut features).unwrap(), "i64");
        assert_eq!(t("text[]", &mut features).unwrap(), "Vec<String>");
        assert_eq!(t("mood", &mut features).unwrap(), "crate::user::Mood");
        assert!(
            t("timestamptz", &mut features)
                .unwrap()
                .contains("DateTime")
        );
        assert!(features.contains(&Feature::Chrono));
        assert_eq!(
            t("numeric(10, 2)", &mut features).unwrap(),
            "rok_db::sqlx::types::Decimal"
        );
        assert!(features.contains(&Feature::Decimal));
        assert_eq!(
            t("numeric[]", &mut features).unwrap(),
            "Vec<rok_db::sqlx::types::Decimal>"
        );
        for interval in ["interval", "interval day to second", "interval(3)"] {
            assert_eq!(
                t(interval, &mut features).unwrap(),
                "rok_db::sqlx::postgres::types::PgInterval"
            );
        }
        assert!(t("money", &mut features).is_err());
        assert!(t("bytea[]", &mut features).is_err());
    }
}
