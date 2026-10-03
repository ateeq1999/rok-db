use std::fmt;
use std::str::FromStr;

use crate::{Error, Value};

/// An opaque position in a keyset-paginated result, returned by
/// [`Select::cursor_paginate`](crate::Select::cursor_paginate).
///
/// A cursor holds the ordering values of the last row of a page. It
/// round-trips through a compact, URL-safe string (`to_string()` /
/// `parse()`), so it can be handed to API clients as a "next page" token.
/// Treat the string as opaque: its format may change between minor versions.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub(crate) values: Vec<Value>,
}

impl Cursor {
    /// A cursor positioned after a row with these ordering values (one per
    /// `ORDER BY` column, in order, including the primary key tiebreaker).
    pub fn new(values: impl IntoIterator<Item = Value>) -> Self {
        Self {
            values: values.into_iter().collect(),
        }
    }

    /// The ordering values this cursor points after.
    pub fn values(&self) -> &[Value] {
        &self.values
    }
}

/// One page of a keyset-paginated query.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorPage<T> {
    /// Records on this page.
    pub items: Vec<T>,
    /// Cursor for the following page, or `None` on the last page.
    pub next: Option<Cursor>,
}

impl<T> CursorPage<T> {
    /// `true` if a later page exists.
    pub fn has_next(&self) -> bool {
        self.next.is_some()
    }

    /// Transform every record, keeping the cursor.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> CursorPage<U> {
        CursorPage {
            items: self.items.into_iter().map(f).collect(),
            next: self.next,
        }
    }
}

impl<T> IntoIterator for CursorPage<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

// ----- encoding --------------------------------------------------------------
//
// `tag` + `~` (NULL) or `tag` + hex(payload), joined with `.`; every character
// is URL-safe.

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Result<Vec<u8>, Error> {
    if s.len() % 2 != 0 {
        return Err(Error::InvalidCursor("odd hex length".into()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| Error::InvalidCursor("bad hex digit".into()))
        })
        .collect()
}

fn text(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|_| Error::InvalidCursor("invalid utf-8".into()))
}

fn parse<T: FromStr>(bytes: Vec<u8>) -> Result<T, Error> {
    text(bytes)?
        .parse()
        .map_err(|_| Error::InvalidCursor("malformed value".into()))
}

fn encode(value: &Value) -> String {
    fn part<T>(tag: char, v: &Option<T>, f: impl FnOnce(&T) -> Vec<u8>) -> String {
        match v {
            Some(v) => format!("{tag}{}", hex(&f(v))),
            None => format!("{tag}~"),
        }
    }
    let s = |v: &dyn ToString| v.to_string().into_bytes();
    match value {
        Value::Bool(v) => part('b', v, |v| s(v)),
        Value::I16(v) => part('h', v, |v| s(v)),
        Value::I32(v) => part('i', v, |v| s(v)),
        Value::I64(v) => part('l', v, |v| s(v)),
        Value::F32(v) => part('f', v, |v| s(&v.to_bits())),
        Value::F64(v) => part('d', v, |v| s(&v.to_bits())),
        Value::String(v) => part('s', v, |v| v.as_bytes().to_vec()),
        Value::Bytes(v) => part('y', v, |v| v.clone()),
        #[cfg(feature = "uuid")]
        Value::Uuid(v) => part('u', v, |v| s(v)),
        #[cfg(feature = "chrono")]
        Value::DateTime(v) => part('t', v, |v| v.to_rfc3339().into_bytes()),
        #[cfg(feature = "chrono")]
        Value::NaiveDateTime(v) => part('n', v, |v| s(v)),
        #[cfg(feature = "chrono")]
        Value::NaiveDate(v) => part('D', v, |v| s(v)),
        #[cfg(feature = "chrono")]
        Value::NaiveTime(v) => part('T', v, |v| s(v)),
        #[cfg(feature = "json")]
        Value::Json(v) => part('j', v, |v| s(v)),
        // Custom types can't be decoded generically; `x` makes the cursor
        // fail to parse with a clear error instead of misbehaving.
        Value::Custom(v) => format!("x{}", hex(format!("{v:?}").as_bytes())),
    }
}

fn decode(part: &str) -> Result<Value, Error> {
    let mut chars = part.chars();
    let tag = chars
        .next()
        .ok_or_else(|| Error::InvalidCursor("empty value".into()))?;
    let rest = chars.as_str();
    let payload = if rest == "~" {
        None
    } else {
        Some(unhex(rest)?)
    };
    macro_rules! val {
        ($variant:ident, |$b:ident| $conv:expr) => {
            Value::$variant(match payload {
                Some($b) => Some($conv),
                None => None,
            })
        };
    }
    Ok(match tag {
        'b' => val!(Bool, |b| parse(b)?),
        'h' => val!(I16, |b| parse(b)?),
        'i' => val!(I32, |b| parse(b)?),
        'l' => val!(I64, |b| parse(b)?),
        'f' => val!(F32, |b| f32::from_bits(parse(b)?)),
        'd' => val!(F64, |b| f64::from_bits(parse(b)?)),
        's' => val!(String, |b| text(b)?),
        'y' => val!(Bytes, |b| b),
        #[cfg(feature = "uuid")]
        'u' => val!(Uuid, |b| parse(b)?),
        #[cfg(feature = "chrono")]
        't' => val!(DateTime, |b| {
            sqlx::types::chrono::DateTime::parse_from_rfc3339(&text(b)?)
                .map_err(|_| Error::InvalidCursor("malformed timestamp".into()))?
                .with_timezone(&sqlx::types::chrono::Utc)
        }),
        #[cfg(feature = "chrono")]
        'n' => val!(NaiveDateTime, |b| parse(b)?),
        #[cfg(feature = "chrono")]
        'D' => val!(NaiveDate, |b| parse(b)?),
        #[cfg(feature = "chrono")]
        'T' => val!(NaiveTime, |b| parse(b)?),
        #[cfg(feature = "json")]
        'j' => val!(Json, |b| serde_json::from_slice(&b)
            .map_err(|_| Error::InvalidCursor("malformed json".into()))?),
        'x' => {
            return Err(Error::InvalidCursor(
                "custom column types can't be used as keyset pagination columns".into(),
            ));
        }
        other => return Err(Error::InvalidCursor(format!("unknown type tag `{other}`"))),
    })
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.values.iter().map(encode).collect();
        f.write_str(&parts.join("."))
    }
}

impl FromStr for Cursor {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Error> {
        if s.is_empty() {
            return Err(Error::InvalidCursor("empty cursor".into()));
        }
        Ok(Self {
            values: s.split('.').map(decode).collect::<Result<_, _>>()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let cursor = Cursor::new([
            Value::from(42_i64),
            Value::from("a.b~c/é"),
            Value::from(None::<i32>),
            Value::from(-1.5_f64),
            Value::from(true),
            Value::from(vec![0_u8, 255]),
        ]);
        let encoded = cursor.to_string();
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ".~".contains(c)),
            "{encoded}"
        );
        assert_eq!(encoded.parse::<Cursor>().unwrap(), cursor);
    }

    #[test]
    fn rejects_garbage() {
        for bad in ["", "x12", "lzz", "l1", "s~.q"] {
            assert!(bad.parse::<Cursor>().is_err(), "{bad}");
        }
    }
}
