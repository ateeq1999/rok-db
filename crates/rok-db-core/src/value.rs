use std::fmt;
use std::sync::Arc;

use sqlx::encode::IsNull;
use sqlx::postgres::{PgArgumentBuffer, PgArguments};
use sqlx::{Arguments, Encode, Postgres, Type};

use crate::Result;

/// A dynamically typed, bindable SQL value.
///
/// Every variant carries an `Option` so that `NULL`s keep their SQL type,
/// which PostgreSQL requires when binding parameters.
///
/// You rarely build a `Value` yourself: anything implementing `Into<Value>`
/// (integers, floats, strings, `bool`, `Vec<u8>`, their `Option`s and, with
/// the matching features, `chrono`, `uuid` and JSON types) can be passed to
/// the query builder directly.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Value {
    /// `BOOLEAN`
    Bool(Option<bool>),
    /// `SMALLINT` (`INT2`)
    I16(Option<i16>),
    /// `INTEGER` (`INT4`)
    I32(Option<i32>),
    /// `BIGINT` (`INT8`)
    I64(Option<i64>),
    /// `REAL` (`FLOAT4`)
    F32(Option<f32>),
    /// `DOUBLE PRECISION` (`FLOAT8`)
    F64(Option<f64>),
    /// `TEXT` / `VARCHAR`
    String(Option<String>),
    /// `BYTEA`
    Bytes(Option<Vec<u8>>),
    #[cfg(feature = "uuid")]
    /// `UUID`
    Uuid(Option<sqlx::types::Uuid>),
    #[cfg(feature = "chrono")]
    /// `TIMESTAMPTZ`
    DateTime(Option<sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>>),
    #[cfg(feature = "chrono")]
    /// `TIMESTAMP`
    NaiveDateTime(Option<sqlx::types::chrono::NaiveDateTime>),
    #[cfg(feature = "chrono")]
    /// `DATE`
    NaiveDate(Option<sqlx::types::chrono::NaiveDate>),
    #[cfg(feature = "chrono")]
    /// `TIME`
    NaiveTime(Option<sqlx::types::chrono::NaiveTime>),
    #[cfg(feature = "json")]
    /// `JSONB` / `JSON`
    Json(Option<serde_json::Value>),
    /// Any other type sqlx can encode: enums and newtypes from
    /// `#[derive(DbEnum)]` / `#[derive(DbNewtype)]`, or types registered
    /// with [`impl_value!`](crate::impl_value). Build one with [`Value::custom`].
    Custom(CustomValue),
}

impl Value {
    /// Wrap any value sqlx can encode for PostgreSQL. `None` binds a `NULL`
    /// of `T`'s SQL type.
    pub fn custom<T>(value: Option<T>) -> Self
    where
        T: for<'q> Encode<'q, Postgres>
            + Type<Postgres>
            + Clone
            + fmt::Debug
            + Send
            + Sync
            + 'static,
    {
        Value::Custom(CustomValue(Arc::new(value)))
    }

    /// `true` if this value is a (typed) SQL `NULL`.
    pub fn is_null(&self) -> bool {
        match self {
            Value::Bool(v) => v.is_none(),
            Value::I16(v) => v.is_none(),
            Value::I32(v) => v.is_none(),
            Value::I64(v) => v.is_none(),
            Value::F32(v) => v.is_none(),
            Value::F64(v) => v.is_none(),
            Value::String(v) => v.is_none(),
            Value::Bytes(v) => v.is_none(),
            #[cfg(feature = "uuid")]
            Value::Uuid(v) => v.is_none(),
            #[cfg(feature = "chrono")]
            Value::DateTime(v) => v.is_none(),
            #[cfg(feature = "chrono")]
            Value::NaiveDateTime(v) => v.is_none(),
            #[cfg(feature = "chrono")]
            Value::NaiveDate(v) => v.is_none(),
            #[cfg(feature = "chrono")]
            Value::NaiveTime(v) => v.is_none(),
            #[cfg(feature = "json")]
            Value::Json(v) => v.is_none(),
            Value::Custom(v) => v.0.is_null(),
        }
    }

    /// Encode in PostgreSQL's binary format; `None` for `NULL`.
    pub(crate) fn encode_binary(&self) -> Result<Option<Vec<u8>>> {
        fn enc<T: for<'q> Encode<'q, Postgres>>(
            v: &T,
            buf: &mut PgArgumentBuffer,
        ) -> std::result::Result<IsNull, sqlx::error::BoxDynError> {
            v.encode_by_ref(buf)
        }
        let mut buf = PgArgumentBuffer::default();
        let is_null = match self {
            Value::Bool(v) => enc(v, &mut buf),
            Value::I16(v) => enc(v, &mut buf),
            Value::I32(v) => enc(v, &mut buf),
            Value::I64(v) => enc(v, &mut buf),
            Value::F32(v) => enc(v, &mut buf),
            Value::F64(v) => enc(v, &mut buf),
            Value::String(v) => enc(v, &mut buf),
            Value::Bytes(v) => enc(v, &mut buf),
            #[cfg(feature = "uuid")]
            Value::Uuid(v) => enc(v, &mut buf),
            #[cfg(feature = "chrono")]
            Value::DateTime(v) => enc(v, &mut buf),
            #[cfg(feature = "chrono")]
            Value::NaiveDateTime(v) => enc(v, &mut buf),
            #[cfg(feature = "chrono")]
            Value::NaiveDate(v) => enc(v, &mut buf),
            #[cfg(feature = "chrono")]
            Value::NaiveTime(v) => enc(v, &mut buf),
            #[cfg(feature = "json")]
            Value::Json(v) => enc(v, &mut buf),
            Value::Custom(v) => v.0.encode(&mut buf),
        }
        .map_err(crate::Error::Encode)?;
        Ok(match is_null {
            IsNull::Yes => None,
            IsNull::No => Some(buf.to_vec()),
        })
    }

    pub(crate) fn bind(self, args: &mut PgArguments) -> Result<()> {
        let res = match self {
            Value::Bool(v) => args.add(v),
            Value::I16(v) => args.add(v),
            Value::I32(v) => args.add(v),
            Value::I64(v) => args.add(v),
            Value::F32(v) => args.add(v),
            Value::F64(v) => args.add(v),
            Value::String(v) => args.add(v),
            Value::Bytes(v) => args.add(v),
            #[cfg(feature = "uuid")]
            Value::Uuid(v) => args.add(v),
            #[cfg(feature = "chrono")]
            Value::DateTime(v) => args.add(v),
            #[cfg(feature = "chrono")]
            Value::NaiveDateTime(v) => args.add(v),
            #[cfg(feature = "chrono")]
            Value::NaiveDate(v) => args.add(v),
            #[cfg(feature = "chrono")]
            Value::NaiveTime(v) => args.add(v),
            #[cfg(feature = "json")]
            Value::Json(v) => args.add(v),
            Value::Custom(v) => v.0.bind(args),
        };
        res.map_err(crate::Error::Encode)
    }
}

/// A value of a user-defined type; see [`Value::custom`].
#[derive(Clone)]
pub struct CustomValue(Arc<dyn DynValue>);

trait DynValue: Send + Sync {
    fn bind(&self, args: &mut PgArguments) -> std::result::Result<(), sqlx::error::BoxDynError>;
    fn encode(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> std::result::Result<IsNull, sqlx::error::BoxDynError>;
    fn is_null(&self) -> bool;
    fn show(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result;
}

impl<T> DynValue for Option<T>
where
    T: for<'q> Encode<'q, Postgres> + Type<Postgres> + Clone + fmt::Debug + Send + Sync + 'static,
{
    fn bind(&self, args: &mut PgArguments) -> std::result::Result<(), sqlx::error::BoxDynError> {
        args.add(self.clone())
    }

    fn encode(
        &self,
        buf: &mut PgArgumentBuffer,
    ) -> std::result::Result<IsNull, sqlx::error::BoxDynError> {
        Encode::<Postgres>::encode_by_ref(self, buf)
    }

    fn is_null(&self) -> bool {
        self.is_none()
    }

    fn show(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Some(v) => write!(f, "{v:?}"),
            None => f.write_str("NULL"),
        }
    }
}

impl fmt::Debug for CustomValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.show(f)
    }
}

impl PartialEq for CustomValue {
    /// Custom values compare by their debug representation.
    fn eq(&self, other: &Self) -> bool {
        format!("{self:?}") == format!("{other:?}")
    }
}

/// Marker for user-defined column types stored through [`Value::custom`].
///
/// Implemented by [`impl_value!`](crate::impl_value), `#[derive(DbEnum)]` and
/// `#[derive(DbNewtype)]`; it unlocks `From<Option<T>>` and
/// `From<&Option<T>>` for [`Value`], which a downstream crate couldn't write
/// itself.
pub trait CustomType:
    for<'q> Encode<'q, Postgres> + Type<Postgres> + Clone + fmt::Debug + Send + Sync + 'static
{
}

impl<T: CustomType> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        Value::custom(v)
    }
}

impl<T: CustomType> From<&Option<T>> for Value {
    fn from(v: &Option<T>) -> Self {
        Value::custom(v.clone())
    }
}

/// Make a type usable as a rok-db column value (in models, filters and
/// `set`), given that it already implements sqlx's `Type` and `Encode` for
/// PostgreSQL plus `Clone + Debug`. Implements [`CustomType`] and
/// `From<T>` / `From<&T>` for [`Value`] (`Option<T>` then works too).
///
/// `#[derive(DbEnum)]` and `#[derive(DbNewtype)]` do this for you.
///
/// ```ignore
/// rok_db::impl_value!(my_crate::Money);
/// ```
#[macro_export]
macro_rules! impl_value {
    ($($ty:ty),+ $(,)?) => {$(
        impl $crate::CustomType for $ty {}
        impl ::core::convert::From<$ty> for $crate::Value {
            fn from(v: $ty) -> Self {
                $crate::Value::custom(::core::option::Option::Some(v))
            }
        }
        impl ::core::convert::From<&$ty> for $crate::Value {
            fn from(v: &$ty) -> Self {
                $crate::Value::custom(::core::option::Option::Some(::core::clone::Clone::clone(v)))
            }
        }
    )+};
}

/// PostgreSQL arrays (`TEXT[]`, `BIGINT[]`, …) as column values.
macro_rules! impl_array {
    ($($t:ty),* $(,)?) => {$(
        impl CustomType for Vec<$t> {}
        impl From<Vec<$t>> for Value {
            fn from(v: Vec<$t>) -> Self {
                Value::custom(Some(v))
            }
        }
        impl From<&Vec<$t>> for Value {
            fn from(v: &Vec<$t>) -> Self {
                Value::custom(Some(v.clone()))
            }
        }
        impl From<&[$t]> for Value {
            fn from(v: &[$t]) -> Self {
                Value::custom(Some(v.to_vec()))
            }
        }
    )*};
}

impl_array!(String, bool, i16, i32, i64, f32, f64);

#[cfg(feature = "uuid")]
impl_array!(sqlx::types::Uuid);

#[cfg(feature = "chrono")]
impl_array!(
    sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    sqlx::types::chrono::NaiveDate,
);

macro_rules! impl_from {
    ($($variant:ident => $ty:ty),* $(,)?) => {$(
        impl From<$ty> for Value {
            fn from(v: $ty) -> Self { Value::$variant(Some(v.into())) }
        }
        impl From<Option<$ty>> for Value {
            fn from(v: Option<$ty>) -> Self { Value::$variant(v.map(Into::into)) }
        }
        impl From<&$ty> for Value {
            fn from(v: &$ty) -> Self { Value::from(v.clone()) }
        }
        impl From<&Option<$ty>> for Value {
            fn from(v: &Option<$ty>) -> Self { Value::from(v.clone()) }
        }
    )*};
}

impl_from! {
    Bool => bool,
    I16 => i16,
    I32 => i32,
    I64 => i64,
    F32 => f32,
    F64 => f64,
    String => String,
    Bytes => Vec<u8>,
}

#[cfg(feature = "uuid")]
impl_from! { Uuid => sqlx::types::Uuid }

#[cfg(feature = "chrono")]
impl_from! {
    DateTime => sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>,
    NaiveDateTime => sqlx::types::chrono::NaiveDateTime,
    NaiveDate => sqlx::types::chrono::NaiveDate,
    NaiveTime => sqlx::types::chrono::NaiveTime,
}

#[cfg(feature = "json")]
impl_from! { Json => serde_json::Value }

#[cfg(feature = "json")]
impl<T: serde::Serialize> From<sqlx::types::Json<T>> for Value {
    fn from(v: sqlx::types::Json<T>) -> Self {
        Value::Json(serde_json::to_value(&v.0).ok())
    }
}

#[cfg(feature = "json")]
impl<T: serde::Serialize> From<&sqlx::types::Json<T>> for Value {
    fn from(v: &sqlx::types::Json<T>) -> Self {
        Value::Json(serde_json::to_value(&v.0).ok())
    }
}

#[cfg(feature = "json")]
impl<T: serde::Serialize> From<&Option<sqlx::types::Json<T>>> for Value {
    fn from(v: &Option<sqlx::types::Json<T>>) -> Self {
        Value::Json(v.as_ref().and_then(|v| serde_json::to_value(&v.0).ok()))
    }
}

#[cfg(feature = "json")]
impl<T: serde::Serialize> From<Option<sqlx::types::Json<T>>> for Value {
    fn from(v: Option<sqlx::types::Json<T>>) -> Self {
        Value::Json(v.and_then(|v| serde_json::to_value(&v.0).ok()))
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(Some(v.to_owned()))
    }
}

impl From<Option<&str>> for Value {
    fn from(v: Option<&str>) -> Self {
        Value::String(v.map(str::to_owned))
    }
}

impl From<&[u8]> for Value {
    fn from(v: &[u8]) -> Self {
        Value::Bytes(Some(v.to_vec()))
    }
}

impl From<i8> for Value {
    fn from(v: i8) -> Self {
        Value::I16(Some(v.into()))
    }
}

impl From<u8> for Value {
    fn from(v: u8) -> Self {
        Value::I16(Some(v.into()))
    }
}

impl From<u16> for Value {
    fn from(v: u16) -> Self {
        Value::I32(Some(v.into()))
    }
}

impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Value::I64(Some(v.into()))
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn show<T: std::fmt::Debug>(
            f: &mut std::fmt::Formatter<'_>,
            v: &Option<T>,
        ) -> std::fmt::Result {
            match v {
                Some(v) => write!(f, "{v:?}"),
                None => f.write_str("NULL"),
            }
        }
        match self {
            Value::Bool(v) => show(f, v),
            Value::I16(v) => show(f, v),
            Value::I32(v) => show(f, v),
            Value::I64(v) => show(f, v),
            Value::F32(v) => show(f, v),
            Value::F64(v) => show(f, v),
            Value::String(v) => show(f, v),
            Value::Bytes(v) => show(f, v),
            #[cfg(feature = "uuid")]
            Value::Uuid(v) => show(f, v),
            #[cfg(feature = "chrono")]
            Value::DateTime(v) => show(f, v),
            #[cfg(feature = "chrono")]
            Value::NaiveDateTime(v) => show(f, v),
            #[cfg(feature = "chrono")]
            Value::NaiveDate(v) => show(f, v),
            #[cfg(feature = "chrono")]
            Value::NaiveTime(v) => show(f, v),
            #[cfg(feature = "json")]
            Value::Json(v) => show(f, v),
            Value::Custom(v) => v.0.show(f),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_keep_types() {
        assert_eq!(Value::from(1_i32), Value::I32(Some(1)));
        assert_eq!(Value::from("a"), Value::String(Some("a".into())));
        assert_eq!(Value::from(None::<i64>), Value::I64(None));
        assert_eq!(Value::from(&Some(true)), Value::Bool(Some(true)));
        assert!(Value::from(None::<String>).is_null());
        assert!(!Value::from(0_u8).is_null());
    }
}
