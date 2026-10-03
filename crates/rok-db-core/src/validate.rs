//! Validation of model fields, run automatically before `insert`, `save`,
//! `upsert` and `insert_many`.
//!
//! Declare rules on fields with `#[rok(validate(…))]`:
//!
//! ```ignore
//! #[derive(Model)]
//! #[rok(validate_with = check_user)]          // whole-record rule
//! struct User {
//!     id: i64,
//!     #[rok(validate(length(min = 1, max = 50)))]
//!     name: String,
//!     #[rok(validate(email))]
//!     email: String,
//!     #[rok(validate(range(min = 0, max = 150)))]
//!     age: i32,
//!     #[rok(validate(custom = no_spaces))]       // fn(&str) -> Result<(), String>
//!     handle: String,
//! }
//!
//! fn check_user(user: &User) -> Result<(), ValidationErrors> { Ok(()) }
//! ```
//!
//! Rules on `Option` fields only apply when the value is `Some`. Failures
//! are collected (not short-circuited) into [`ValidationErrors`] and
//! returned as [`Error::Validation`](crate::Error::Validation).

use std::fmt;

/// One failed rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// Field name (`""` for record-level errors).
    pub field: &'static str,
    /// Machine-readable rule name: `length`, `range`, `email`,
    /// `non_empty`, `custom`, …
    pub code: &'static str,
    /// Human-readable message.
    pub message: String,
}

/// Every validation failure of a record.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationErrors {
    errors: Vec<FieldError>,
}

impl ValidationErrors {
    /// No errors yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failure.
    pub fn add(&mut self, field: &'static str, code: &'static str, message: impl Into<String>) {
        self.errors.push(FieldError {
            field,
            code,
            message: message.into(),
        });
    }

    /// Append every error of `other`.
    pub fn merge(&mut self, other: ValidationErrors) {
        self.errors.extend(other.errors);
    }

    /// `true` if nothing failed.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// All failures, in declaration order.
    pub fn errors(&self) -> &[FieldError] {
        &self.errors
    }

    /// Failures of one field.
    pub fn field(&self, field: &str) -> impl Iterator<Item = &FieldError> {
        self.errors.iter().filter(move |e| e.field == field)
    }

    /// `Ok(())` if empty, `Err(self)` otherwise.
    pub fn into_result(self) -> Result<(), ValidationErrors> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }
}

impl fmt::Display for ValidationErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.errors.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            if e.field.is_empty() {
                f.write_str(&e.message)?;
            } else {
                write!(f, "{}: {}", e.field, e.message)?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for ValidationErrors {}

/// Values with a length; `None` means "absent, skip the rule".
pub trait HasLength {
    /// Length in characters (strings) or elements (collections).
    fn length(&self) -> Option<usize>;
}

impl HasLength for str {
    fn length(&self) -> Option<usize> {
        Some(self.chars().count())
    }
}

impl HasLength for String {
    fn length(&self) -> Option<usize> {
        self.as_str().length()
    }
}

impl<T> HasLength for Vec<T> {
    fn length(&self) -> Option<usize> {
        Some(self.len())
    }
}

impl<T: HasLength + ?Sized> HasLength for &T {
    fn length(&self) -> Option<usize> {
        (**self).length()
    }
}

impl<T: HasLength> HasLength for Option<T> {
    fn length(&self) -> Option<usize> {
        self.as_ref().and_then(HasLength::length)
    }
}

/// Numeric values; `None` means "absent, skip the rule".
pub trait AsNumber {
    /// The value as `f64`.
    fn as_number(&self) -> Option<f64>;
}

macro_rules! as_number {
    ($($t:ty),*) => {$(
        impl AsNumber for $t {
            fn as_number(&self) -> Option<f64> {
                Some(*self as f64)
            }
        }
    )*};
}

as_number!(i8, i16, i32, i64, u8, u16, u32, u64, f32, f64);

impl<T: AsNumber> AsNumber for Option<T> {
    fn as_number(&self) -> Option<f64> {
        self.as_ref().and_then(AsNumber::as_number)
    }
}

/// Text values; `None` means "absent, skip the rule".
pub trait AsText {
    /// The value as `&str`.
    fn as_text(&self) -> Option<&str>;
}

impl AsText for String {
    fn as_text(&self) -> Option<&str> {
        Some(self)
    }
}

impl AsText for str {
    fn as_text(&self) -> Option<&str> {
        Some(self)
    }
}

impl<T: AsText> AsText for Option<T> {
    fn as_text(&self) -> Option<&str> {
        self.as_ref().and_then(AsText::as_text)
    }
}

/// `length(min, max)` rule.
pub fn length<T: HasLength + ?Sized>(
    value: &T,
    min: Option<usize>,
    max: Option<usize>,
) -> Result<(), String> {
    let Some(len) = value.length() else {
        return Ok(());
    };
    match (min, max) {
        (Some(min), _) if len < min => Err(match max {
            Some(max) => format!("length must be between {min} and {max}"),
            None => format!("length must be at least {min}"),
        }),
        (_, Some(max)) if len > max => Err(match min {
            Some(min) => format!("length must be between {min} and {max}"),
            None => format!("length must be at most {max}"),
        }),
        _ => Ok(()),
    }
}

/// `non_empty` rule (`length(min = 1)` with a clearer message).
pub fn non_empty<T: HasLength + ?Sized>(value: &T) -> Result<(), String> {
    match value.length() {
        Some(0) => Err("must not be empty".into()),
        _ => Ok(()),
    }
}

/// `range(min, max)` rule (inclusive).
pub fn range<T: AsNumber + ?Sized>(
    value: &T,
    min: Option<f64>,
    max: Option<f64>,
) -> Result<(), String> {
    let Some(n) = value.as_number() else {
        return Ok(());
    };
    let fmt = |x: f64| {
        if x.fract() == 0.0 {
            format!("{x:.0}")
        } else {
            x.to_string()
        }
    };
    match (min, max) {
        (Some(lo), Some(hi)) if n < lo || n > hi => {
            Err(format!("must be between {} and {}", fmt(lo), fmt(hi)))
        }
        (Some(lo), None) if n < lo => Err(format!("must be at least {}", fmt(lo))),
        (None, Some(hi)) if n > hi => Err(format!("must be at most {}", fmt(hi))),
        _ => Ok(()),
    }
}

/// `email` rule: a pragmatic syntax check (`local@domain.tld`, no
/// whitespace), not full RFC 5322.
pub fn email<T: AsText + ?Sized>(value: &T) -> Result<(), String> {
    let Some(s) = value.as_text() else {
        return Ok(());
    };
    let valid = match s.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty()
                && !domain.contains('@')
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !s.chars().any(char::is_whitespace)
        }
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err("must be a valid email address".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert!(length("abc", Some(1), Some(3)).is_ok());
        assert_eq!(
            length("", Some(1), Some(3)).unwrap_err(),
            "length must be between 1 and 3"
        );
        assert_eq!(
            length(&vec![1, 2], None, Some(1)).unwrap_err(),
            "length must be at most 1"
        );
        assert!(
            length(&None::<String>, Some(5), None).is_ok(),
            "None is skipped"
        );
        assert_eq!(length("héllo", None, Some(5)), Ok(()), "counts characters");
        assert!(non_empty("").is_err() && non_empty("x").is_ok());
        assert!(range(&5, Some(0.0), Some(10.0)).is_ok());
        assert_eq!(
            range(&-1, Some(0.0), None).unwrap_err(),
            "must be at least 0"
        );
        assert_eq!(
            range(&2.5_f64, None, Some(2.0)).unwrap_err(),
            "must be at most 2"
        );
        assert!(range(&Some(11), Some(0.0), Some(10.0)).is_err());
        for ok in ["a@b.co", "first.last+tag@sub.example.org"] {
            assert!(email(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "a", "a@b", "@b.co", "a@.co", "a@b.", "a b@c.de", "a@b@c.de",
        ] {
            assert!(email(bad).is_err(), "{bad}");
        }
        assert!(email(&None::<String>).is_ok());
    }

    #[test]
    fn errors_collect_and_display() {
        let mut errors = ValidationErrors::new();
        errors.add("name", "length", "too long");
        errors.add("", "custom", "dates are inverted");
        assert_eq!(errors.field("name").count(), 1);
        assert_eq!(errors.to_string(), "name: too long; dates are inverted");
        assert!(errors.into_result().is_err());
        assert!(ValidationErrors::new().into_result().is_ok());
    }
}
