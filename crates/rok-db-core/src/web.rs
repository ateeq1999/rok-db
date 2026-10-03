//! Web integration: serde support (feature `serde`) and axum responses
//! (feature `axum`).
//!
//! With `axum`, handlers can return `rok_db::Result<T>` (or use `?` on rok-db
//! calls) and errors become JSON responses:
//!
//! | error | status | `error` code |
//! |---|---|---|
//! | `NotFound` | 404 | `not_found` |
//! | `Validation` | 422 | `validation_failed` (with `fields`) |
//! | `Hook` | 422 | `rejected` |
//! | foreign-key violation | 422 | `invalid_reference` |
//! | `Conflict` | 409 | `conflict` |
//! | unique violation | 409 | `already_exists` |
//! | `InvalidCursor` | 400 | `invalid_cursor` |
//! | anything else | 500 | `internal_error` (details are logged, not returned) |
//!
//! ```ignore
//! async fn show(State(db): State<Db>, Path(id): Path<i64>) -> rok_db::Result<Json<User>> {
//!     Ok(Json(User::find_or_fail(&db, id).await?))
//! }
//! ```

#[cfg(feature = "serde")]
mod serde_impls {
    use serde::ser::SerializeStruct;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    use crate::validate::{FieldError, ValidationErrors};
    use crate::{Cursor, CursorPage, Page};

    /// `{"items": […], "total", "page", "per_page", "total_pages"}`
    impl<T: Serialize> Serialize for Page<T> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("Page", 5)?;
            st.serialize_field("items", &self.items)?;
            st.serialize_field("total", &self.total)?;
            st.serialize_field("page", &self.page)?;
            st.serialize_field("per_page", &self.per_page)?;
            st.serialize_field("total_pages", &self.total_pages())?;
            st.end()
        }
    }

    /// `{"items": […], "next": "<cursor>" | null}`
    impl<T: Serialize> Serialize for CursorPage<T> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("CursorPage", 2)?;
            st.serialize_field("items", &self.items)?;
            st.serialize_field("next", &self.next)?;
            st.end()
        }
    }

    /// Serialized as its opaque string form.
    impl Serialize for Cursor {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.collect_str(self)
        }
    }

    /// Parsed from its string form, e.g. a `?after=` query parameter.
    impl<'de> Deserialize<'de> for Cursor {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = String::deserialize(d)?;
            s.parse().map_err(serde::de::Error::custom)
        }
    }

    impl Serialize for FieldError {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("FieldError", 3)?;
            st.serialize_field("field", self.field)?;
            st.serialize_field("code", self.code)?;
            st.serialize_field("message", &self.message)?;
            st.end()
        }
    }

    /// A list of `{"field", "code", "message"}`.
    impl Serialize for ValidationErrors {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            self.errors().serialize(s)
        }
    }
}

#[cfg(feature = "axum")]
mod axum_impls {
    use axum_core::body::Body;
    use axum_core::response::{IntoResponse, Response};
    use http::{StatusCode, header};

    use crate::Error;

    impl Error {
        /// The HTTP status and machine-readable code this error maps to.
        pub fn http_status(&self) -> (StatusCode, &'static str) {
            match self {
                Error::NotFound { .. } => (StatusCode::NOT_FOUND, "not_found"),
                Error::Validation(_) => (StatusCode::UNPROCESSABLE_ENTITY, "validation_failed"),
                Error::Hook(_) => (StatusCode::UNPROCESSABLE_ENTITY, "rejected"),
                Error::Conflict { .. } => (StatusCode::CONFLICT, "conflict"),
                Error::InvalidCursor(_) => (StatusCode::BAD_REQUEST, "invalid_cursor"),
                e if e.is_not_found() => (StatusCode::NOT_FOUND, "not_found"),
                e if e.is_unique_violation() => (StatusCode::CONFLICT, "already_exists"),
                e if e.is_foreign_key_violation() => {
                    (StatusCode::UNPROCESSABLE_ENTITY, "invalid_reference")
                }
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
            }
        }
    }

    impl IntoResponse for Error {
        fn into_response(self) -> Response {
            let (status, code) = self.http_status();
            let message = match code {
                "internal_error" => {
                    tracing::error!(target: "rok_db::http", error = %self, "internal error");
                    "internal server error".to_owned()
                }
                "already_exists" => "a record with these values already exists".to_owned(),
                "invalid_reference" => "a referenced record does not exist".to_owned(),
                _ => self.to_string(),
            };
            let mut body = serde_json::json!({ "error": code, "message": message });
            if let Error::Validation(errors) = &self {
                body["fields"] = serde_json::to_value(errors).unwrap_or_default();
            }
            let mut response = Response::new(Body::from(body.to_string()));
            *response.status_mut() = status;
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            response
        }
    }
}
