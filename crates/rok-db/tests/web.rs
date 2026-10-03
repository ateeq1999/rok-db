//! serde and axum integration.
#![cfg(all(feature = "serde", feature = "axum"))]

use axum_core::response::IntoResponse;
use http_body_util::BodyExt;
use rok_db::{Cursor, CursorPage, Error, Page, ValidationErrors};
use serde_json::{Value as Json, json};

#[test]
fn pages_and_cursors_serialize() {
    let page = Page::new(vec!["a", "b"], 5, 1, 2);
    assert_eq!(
        serde_json::to_value(&page).unwrap(),
        json!({"items": ["a", "b"], "total": 5, "page": 1, "per_page": 2, "total_pages": 3})
    );

    let cursor = Cursor::new([rok_db::Value::from(7_i64)]);
    let page = CursorPage {
        items: vec![1, 2],
        next: Some(cursor.clone()),
    };
    let value = serde_json::to_value(&page).unwrap();
    assert_eq!(value["next"], json!(cursor.to_string()));
    let parsed: Cursor = serde_json::from_value(value["next"].clone()).unwrap();
    assert_eq!(parsed, cursor);
    assert!(serde_json::from_value::<Cursor>(json!("garbage!")).is_err());
    let last = CursorPage::<i32> {
        items: vec![],
        next: None,
    };
    assert_eq!(
        serde_json::to_value(&last).unwrap(),
        json!({"items": [], "next": null})
    );
}

async fn respond(err: Error) -> (u16, Json) {
    let response = err.into_response();
    let status = response.status().as_u16();
    assert_eq!(response.headers()["content-type"], "application/json");
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn errors_become_json_responses() {
    let (status, body) = respond(Error::NotFound {
        table: "users",
        key: Some("7".into()),
    })
    .await;
    assert_eq!((status, body["error"].as_str()), (404, Some("not_found")));
    assert!(body["message"].as_str().unwrap().contains("users"));

    let mut errors = ValidationErrors::new();
    errors.add("email", "email", "must be a valid email address");
    let (status, body) = respond(Error::Validation(errors)).await;
    assert_eq!(status, 422);
    assert_eq!(
        body["fields"],
        json!([{"field": "email", "code": "email", "message": "must be a valid email address"}])
    );

    let (status, body) = respond(Error::Conflict {
        table: "users",
        key: "1".into(),
    })
    .await;
    assert_eq!((status, body["error"].as_str()), (409, Some("conflict")));
    let (status, _) = respond(Error::InvalidCursor("bad".into())).await;
    assert_eq!(status, 400);
    let (status, body) = respond(Error::hook("not allowed")).await;
    assert_eq!(
        (status, body["message"].as_str()),
        (422, Some("not allowed"))
    );

    // Internal details are not leaked.
    let (status, body) = respond(Error::Config("secret connection string".into())).await;
    assert_eq!(status, 500);
    assert_eq!(
        body,
        json!({"error": "internal_error", "message": "internal server error"})
    );
}

#[tokio::test]
async fn database_errors_map_to_statuses() {
    let Some(test_db) = rok_db::testing::TestDb::create().await.unwrap() else {
        return;
    };
    let db = test_db.db();
    db.execute("CREATE TABLE parents (id INT PRIMARY KEY); CREATE TABLE kids (id INT PRIMARY KEY, parent INT REFERENCES parents)")
        .await
        .unwrap();
    rok_db::raw("INSERT INTO parents VALUES (1)")
        .execute(db)
        .await
        .unwrap();
    let dup = rok_db::raw("INSERT INTO parents VALUES (1)")
        .execute(db)
        .await
        .unwrap_err();
    let (status, body) = respond(dup).await;
    assert_eq!(
        (status, body["error"].as_str()),
        (409, Some("already_exists"))
    );
    assert!(
        !body["message"].as_str().unwrap().contains("parents_pkey"),
        "constraint names stay private"
    );
    let orphan = rok_db::raw("INSERT INTO kids VALUES (1, 42)")
        .execute(db)
        .await
        .unwrap_err();
    assert_eq!(respond(orphan).await.0, 422);
}
