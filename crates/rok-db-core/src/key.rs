use crate::{Column, Error, Expr, Model, Result, Value};

/// A primary key value: a single value, or a tuple for models with a
/// composite primary key (`#[rok(primary_key)]` on several fields).
///
/// ```ignore
/// User::find(&db, 42).await?;                       // single key
/// Membership::find(&db, (org_id, user_id)).await?;  // composite key, in field order
/// ```
pub trait IntoKey {
    /// The key's values, in primary-key column order.
    fn into_key(self) -> Vec<Value>;
}

impl<T: Into<Value>> IntoKey for T {
    fn into_key(self) -> Vec<Value> {
        vec![self.into()]
    }
}

macro_rules! tuple_keys {
    ($(($($t:ident),+)),+) => {$(
        #[allow(non_snake_case)]
        impl<$($t: Into<Value>),+> IntoKey for ($($t,)+) {
            fn into_key(self) -> Vec<Value> {
                let ($($t,)+) = self;
                vec![$($t.into()),+]
            }
        }
    )+};
}

tuple_keys!((A, B), (A, B, C), (A, B, C, D));

/// `pk1 = v1 AND pk2 = v2 …` for model `M`.
pub(crate) fn key_expr<M: Model>(values: Vec<Value>) -> Result<Expr<M>> {
    if values.len() != M::PRIMARY_KEYS.len() {
        return Err(Error::InvalidQuery(format!(
            "`{}` has a {}-column primary key, got {} value(s)",
            M::TABLE,
            M::PRIMARY_KEYS.len(),
            values.len()
        )));
    }
    Ok(Expr::all_of(
        M::PRIMARY_KEYS
            .iter()
            .zip(values)
            .map(|(c, v)| Column::<M>::new(c).eq(v)),
    ))
}

/// Human-readable key for error messages: `42` or `(1, 2)`.
pub(crate) fn key_string(values: &[Value]) -> String {
    match values {
        [single] => single.to_string(),
        many => format!(
            "({})",
            many.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// SQL rendering the key of the current row of `M` exactly as the change
/// and audit triggers store it: `to_jsonb(row) ->> 'pk'` for a single key,
/// a JSON array of those texts for composite keys.
pub(crate) fn key_text_sql<M: Model>() -> String {
    let table = M::TABLE.rsplit('.').next().unwrap_or(M::TABLE);
    let mut row = String::new();
    crate::sql::push_ident(&mut row, table);
    let field = |c: &str| format!("(to_jsonb({row}) ->> '{}')", c.replace('\'', "''"));
    match M::PRIMARY_KEYS {
        [single] => field(single),
        many => format!(
            "to_jsonb(ARRAY[{}])::text",
            many.iter().map(|c| field(c)).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// SQL comparing a stored key text against bound key values: `?::text` or
/// `to_jsonb(ARRAY[?::text, …])::text`.
#[cfg_attr(not(feature = "json"), allow(dead_code))]
pub(crate) fn key_param_sql(len: usize) -> String {
    match len {
        1 => "?::text".to_owned(),
        n => format!("to_jsonb(ARRAY[{}])::text", vec!["?::text"; n].join(", ")),
    }
}

/// The key column list passed to triggers.
pub(crate) fn key_columns_arg<M: Model>() -> String {
    M::PRIMARY_KEYS.join(",")
}

/// Trigger-side counterpart of [`key_text_sql`]: `keys` is the
/// comma-separated key column list passed as a trigger argument and `j` the
/// row as JSONB.
pub(crate) const TRIGGER_KEY_SQL: &str = "CASE WHEN cardinality(keys) = 1 THEN j ->> keys[1] \
     ELSE (SELECT to_jsonb(array_agg(j ->> c ORDER BY o))::text FROM unnest(keys) WITH ORDINALITY AS t(c, o)) END";
