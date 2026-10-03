//! SQL generation tests — no database required.

use rok_db::prelude::*;
use rok_db::{__private, Value};

#[derive(Debug, Clone, Model)]
struct User {
    #[rok(primary_key, generated)]
    id: i64,
    email: String,
    name: Option<String>,
    age: i32,
}

#[derive(Debug, Clone, Model)]
#[rok(table = "app.blog_posts")]
struct Post {
    #[rok(primary_key)]
    slug: String,
    #[rok(column = "author_id")]
    author: i64,
    r#type: String,
    #[rok(generated)]
    created_at: i64,
    #[rok(skip)]
    cached_html: Option<String>,
}

#[derive(Debug, Model)]
struct Category {
    id: i32,
    label: String,
}

#[test]
fn derives_metadata() {
    assert_eq!(User::TABLE, "users");
    assert_eq!(User::PRIMARY_KEY, "id");
    assert_eq!(User::COLUMNS, ["id", "email", "name", "age"]);
    assert_eq!(User::GENERATED, ["id"]);

    assert_eq!(Post::TABLE, "app.blog_posts");
    assert_eq!(Post::PRIMARY_KEY, "slug");
    assert_eq!(Post::COLUMNS, ["slug", "author_id", "type", "created_at"]);
    assert_eq!(Post::AUTHOR.name(), "author_id");
    assert_eq!(Post::TYPE.name(), "type");

    assert_eq!(Category::TABLE, "categories");
    assert_eq!(Category::PRIMARY_KEY, "id");
    assert!(Category::GENERATED.is_empty());
}

#[test]
fn model_values() {
    let post = Post {
        slug: "hello".into(),
        author: 7,
        r#type: "article".into(),
        created_at: 0,
        cached_html: Some("<p>".into()),
    };
    assert_eq!(post.primary_key(), Value::from("hello"));
    let values = post.values();
    assert_eq!(values.len(), 4);
    assert_eq!(values[1], ("author_id", Value::from(7_i64)));
    assert!(post.cached_html.is_some(), "skipped fields are not columns");
}

#[test]
fn select_sql() {
    let sql = User::filter(User::AGE.gte(18))
        .filter(User::NAME.is_null().or(User::EMAIL.contains("@example")))
        .order_by(User::AGE.desc().nulls_last())
        .order_by(User::ID)
        .limit(10)
        .offset(20)
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "id", "email", "name", "age" FROM "users" WHERE "age" >= $1 AND ("name" IS NULL OR "email" LIKE $2) ORDER BY "age" DESC NULLS LAST, "id" ASC LIMIT 10 OFFSET 20"#
    );
    assert_eq!(sql.params(), [Value::from(18), Value::from("%@example%")]);
}

#[test]
fn optional_filters() {
    let role: Option<&str> = None;
    let min_age = Some(21);
    let sql = User::query()
        .filter_opt(role, |r| User::NAME.eq(r))
        .filter_opt(min_age, |a| User::AGE.gte(a))
        .filter_if(false, || User::ID.eq(1))
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "id", "email", "name", "age" FROM "users" WHERE "age" >= $1"#
    );
}

#[test]
fn schema_qualified_and_locking() {
    let sql = Post::filter(Post::SLUG.eq("x")).for_update().to_sql();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "slug", "author_id", "type", "created_at" FROM "app"."blog_posts" WHERE "slug" = $1 FOR UPDATE"#
    );
}

#[test]
fn update_and_delete_sql() {
    let sql = User::filter(User::ID.is_in([1_i64, 2, 3]))
        .update()
        .set(User::NAME, "x")
        .increment(User::AGE, 1)
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"UPDATE "users" SET "name" = $1, "age" = "age" + $2 WHERE "id" IN ($3, $4, $5)"#
    );

    let sql = User::filter(!User::AGE.between(1, 2)).delete_sql();
    assert_eq!(
        sql.as_str(),
        r#"DELETE FROM "users" WHERE NOT ("age" BETWEEN $1 AND $2)"#
    );
}

#[test]
fn insert_sql() {
    let users = [
        User {
            id: 0,
            email: "a@x".into(),
            name: None,
            age: 1,
        },
        User {
            id: 0,
            email: "b@x".into(),
            name: Some("B".into()),
            age: 2,
        },
    ];
    let sql = __private::__insert_sql(&users, false);
    assert_eq!(
        sql.as_str(),
        r#"INSERT INTO "users" ("email", "name", "age") VALUES ($1, $2, $3), ($4, $5, $6) RETURNING "id", "email", "name", "age""#
    );
    assert_eq!(sql.params()[1], Value::String(None));

    let sql = __private::__insert_sql(&users[..1], true);
    assert_eq!(
        sql.as_str(),
        r#"INSERT INTO "users" ("id", "email", "name", "age") VALUES ($1, $2, $3, $4) ON CONFLICT ("id") DO UPDATE SET "email" = EXCLUDED."email", "name" = EXCLUDED."name", "age" = EXCLUDED."age" RETURNING "id", "email", "name", "age""#
    );

    let sql = User::create()
        .set(User::EMAIL, "c@x")
        .set(User::AGE, 3)
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"INSERT INTO "users" ("email", "age") VALUES ($1, $2) RETURNING "id", "email", "name", "age""#
    );
}

#[test]
fn paginate_sql() {
    let select = User::filter(User::AGE.gt(1)).order_by(User::ID.desc());
    let sql = __private::__paginate_sql(&select, 3, 10);
    assert_eq!(
        sql.as_str(),
        r#"SELECT c."__rok_total", p.* FROM (SELECT COUNT(*) AS "__rok_total" FROM "users" WHERE "age" > $1) c LEFT JOIN LATERAL (SELECT TRUE AS "__rok_present", "id", "email", "name", "age" FROM "users" WHERE "age" > $2 ORDER BY "id" DESC LIMIT 10 OFFSET 20) p ON TRUE ORDER BY p."id" DESC"#
    );
}

#[test]
fn raw_sql() {
    let sql = Expr::<User>::raw("lower(email) = lower(?)", ["A@X"]);
    let sql = User::filter(sql).filter(User::AGE.eq(3)).to_sql();
    assert!(
        sql.as_str()
            .ends_with(r#"WHERE (lower(email) = lower($1)) AND "age" = $2"#)
    );

    let raw = rok_db::raw("SELECT * FROM users WHERE id = ?").bind(5);
    assert_eq!(raw.to_sql().as_str(), "SELECT * FROM users WHERE id = $1");
}
