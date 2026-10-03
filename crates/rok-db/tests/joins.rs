//! Typed joins (RFC 0001).
#![cfg(feature = "testing")]

use rok_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(has_many(posts = Post::AUTHOR_ID))]
struct User {
    #[rok(generated)]
    id: i64,
    name: String,
    role: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(soft_delete, has_many(comments = Comment::POST_ID))]
struct Post {
    #[rok(generated)]
    id: i64,
    #[rok(belongs_to(author = User))]
    author_id: i64,
    category_id: Option<i64>,
    title: String,
    views: i32,
    deleted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Model)]
#[rok(table = "categories")]
struct Category {
    #[rok(generated)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Model)]
struct Comment {
    #[rok(generated)]
    id: i64,
    #[rok(belongs_to = Post)]
    post_id: i64,
    body: String,
}

const SCHEMA: &str = "
    CREATE TABLE users (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL);
    CREATE TABLE categories (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL);
    CREATE TABLE posts (
        id BIGSERIAL PRIMARY KEY, author_id BIGINT NOT NULL REFERENCES users (id),
        category_id BIGINT REFERENCES categories (id), title TEXT NOT NULL,
        views INT NOT NULL, deleted_at TEXT
    );
    CREATE TABLE comments (id BIGSERIAL PRIMARY KEY, post_id BIGINT NOT NULL REFERENCES posts (id), body TEXT NOT NULL);
";

#[test]
fn sql_shape() {
    let sql = Post::query()
        .join(Post::AUTHOR)
        .filter(User::ROLE.eq("admin"))
        .filter(Post::VIEWS.gt(10))
        .order_by(User::NAME.asc())
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "posts"."id", "posts"."author_id", "posts"."category_id", "posts"."title", "posts"."views", "posts"."deleted_at" FROM "posts" INNER JOIN "users" ON "users"."id" = "posts"."author_id" WHERE "users"."role" = $1 AND "posts"."views" > $2 AND "posts"."deleted_at" IS NULL ORDER BY "users"."name" ASC"#
    );

    // Multiplying joins de-duplicate root rows and keep the ordering.
    let sql = User::query()
        .join(User::POSTS)
        .order_by(Post::VIEWS.desc())
        .limit(5)
        .to_sql();
    assert_eq!(
        sql.as_str(),
        r#"SELECT "__rok_d".* FROM (SELECT DISTINCT ON ("users"."id") "users"."id", "users"."name", "users"."role", "posts"."views" AS "__rok_o0" FROM "users" INNER JOIN "posts" ON "posts"."author_id" = "users"."id" AND "posts"."deleted_at" IS NULL ORDER BY "users"."id", "posts"."views" DESC) "__rok_d" ORDER BY "__rok_d"."__rok_o0" DESC LIMIT 5"#
    );

    // Join-free queries are unchanged.
    assert_eq!(
        User::filter(User::ROLE.eq("x")).to_sql().as_str(),
        r#"SELECT "id", "name", "role" FROM "users" WHERE "role" = $1"#
    );
}

async fn seed(db: &Db) -> (Vec<User>, Vec<Post>) {
    db.execute(SCHEMA).await.unwrap();
    let u = |name: &str, role: &str| User {
        id: 0,
        name: name.into(),
        role: role.into(),
    };
    let users = User::insert_all(
        db,
        &[u("ann", "admin"), u("bob", "member"), u("cid", "admin")],
    )
    .await
    .unwrap();
    let rust = Category::create()
        .set(Category::NAME, "rust")
        .exec(db)
        .await
        .unwrap();
    let p = |author: &User, cat: Option<&Category>, title: &str, views: i32| Post {
        id: 0,
        author_id: author.id,
        category_id: cat.map(|c| c.id),
        title: title.into(),
        views,
        deleted_at: None,
    };
    let posts = Post::insert_all(
        db,
        &[
            p(&users[0], Some(&rust), "a1", 100),
            p(&users[0], None, "a2", 5),
            p(&users[1], Some(&rust), "b1", 50),
            p(&users[1], None, "b2", 500),
        ],
    )
    .await
    .unwrap();
    // cid has no posts; a soft-deleted post must be invisible through joins.
    let hidden = Post::insert_all(db, &[p(&users[2], None, "gone", 9999)])
        .await
        .unwrap();
    hidden[0].delete(db).await.unwrap();
    Comment::create()
        .set(Comment::POST_ID, posts[0].id)
        .set(Comment::BODY, "nice")
        .exec(db)
        .await
        .unwrap();
    (users, posts)
}

#[rok_db::test]
async fn belongs_to_joins(db: Db) {
    seed(&db).await;
    let admin_posts = Post::query()
        .join(Post::AUTHOR)
        .filter(User::ROLE.eq("admin"))
        .order_by(Post::TITLE)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        admin_posts
            .iter()
            .map(|p| p.title.as_str())
            .collect::<Vec<_>>(),
        ["a1", "a2"]
    );

    let rows: Vec<(String, String, i32)> = Post::query()
        .join(Post::AUTHOR)
        .order_by(User::NAME.desc())
        .order_by(Post::VIEWS.desc())
        .select((User::NAME, Post::TITLE, Post::VIEWS))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(rows[0], ("bob".to_string(), "b2".to_string(), 500));
    assert_eq!(rows.len(), 4);
}

#[rok_db::test]
async fn has_many_joins_deduplicate(db: Db) {
    let (users, _) = seed(&db).await;

    // Users with a popular post; ann has one, bob has two (counted once).
    let popular = User::query()
        .join(User::POSTS)
        .filter(Post::VIEWS.gte(50))
        .order_by(User::ID);
    let found = popular.clone().all(&db).await.unwrap();
    assert_eq!(
        found.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
        ["ann", "bob"]
    );
    assert_eq!(popular.clone().count(&db).await.unwrap(), 2);
    assert!(popular.clone().exists(&db).await.unwrap());

    // Order by a joined column: each user ranked by their top post.
    let ranked = User::query()
        .join(User::POSTS)
        .order_by(Post::VIEWS.desc())
        .all(&db)
        .await
        .unwrap();
    assert_eq!(
        ranked.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
        ["bob", "ann"]
    );

    let page = User::query()
        .join(User::POSTS)
        .order_by(Post::VIEWS.desc())
        .paginate(&db, 1, 1)
        .await
        .unwrap();
    assert_eq!((page.total, page.items[0].name.as_str()), (2, "bob"));
    let page2 = User::query()
        .join(User::POSTS)
        .order_by(Post::VIEWS.desc())
        .paginate(&db, 2, 1)
        .await
        .unwrap();
    assert_eq!(page2.items[0].name, "ann");

    // The soft-deleted post of cid is excluded by the join's ON clause.
    assert!(
        User::query()
            .join(User::POSTS)
            .filter(User::ID.eq(users[2].id))
            .first(&db)
            .await
            .unwrap()
            .is_none()
    );
}

#[rok_db::test]
async fn left_joins_and_grouping(db: Db) {
    seed(&db).await;
    let per_user: Vec<(String, i64)> = User::query()
        .left_join(User::POSTS)
        .group_by(User::NAME)
        .order_by(User::NAME)
        .select((User::NAME, Post::ID.count()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(
        per_user,
        [("ann".into(), 2), ("bob".into(), 2), ("cid".into(), 0)]
    );

    let per_category: Vec<(Option<String>, i64)> = Post::query()
        .left_join(Category::ID.on(Post::CATEGORY_ID))
        .group_by(Category::NAME)
        .order_by(Category::NAME.asc().nulls_last())
        .select((Category::NAME, Projection::<Post>::count_all()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(per_category, [(Some("rust".into()), 2), (None, 2)]);
}

#[rok_db::test]
async fn chained_joins(db: Db) {
    seed(&db).await;
    // Comment -> Post -> User, filtering on the last model.
    let by_admins: Vec<(String, String)> = Comment::query()
        .join(Comment::POST)
        .join(Post::AUTHOR)
        .filter(User::ROLE.eq("admin"))
        .select((Comment::BODY, User::NAME))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(by_admins, [("nice".to_string(), "ann".to_string())]);

    // Users who received comments, through two has_many joins.
    let n = User::query()
        .join(User::POSTS)
        .join(Post::COMMENTS)
        .count(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn cursor_sql_shape() {
    let sql = rok_db::__private::__cursor_sql(
        Post::query()
            .join(Post::AUTHOR)
            .order_by(User::NAME.asc())
            .into_select(),
        None,
        2,
    )
    .unwrap();
    assert_eq!(
        sql.as_str(),
        r#"SELECT * FROM (SELECT "posts"."id", "posts"."author_id", "posts"."category_id", "posts"."title", "posts"."views", "posts"."deleted_at", "users"."name" AS "__rok_o0", "posts"."id" AS "__rok_o1" FROM "posts" INNER JOIN "users" ON "users"."id" = "posts"."author_id" WHERE "posts"."deleted_at" IS NULL) "__rok_k" ORDER BY "__rok_k"."__rok_o0" ASC, "__rok_k"."__rok_o1" ASC LIMIT 3"#
    );
}

#[rok_db::test]
async fn joined_cursor_pagination(db: Db) {
    seed(&db).await;
    // Posts ordered by author name, then views (descending).
    let query = || {
        Post::query()
            .join(Post::AUTHOR)
            .order_by(User::NAME.asc())
            .order_by(Post::VIEWS.desc())
    };
    let mut titles = Vec::new();
    let mut after = None;
    loop {
        let page = query()
            .cursor_paginate(&db, after.as_ref(), 3)
            .await
            .unwrap();
        titles.extend(page.items.into_iter().map(|p| p.title));
        match page.next {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    assert_eq!(titles, ["a1", "a2", "b2", "b1"]);

    // A multiplying join: users ranked by their top post, one page at a time.
    let first = User::query()
        .join(User::POSTS)
        .order_by(Post::VIEWS.desc())
        .cursor_paginate(&db, None, 1)
        .await
        .unwrap();
    assert_eq!(first.items[0].name, "bob");
    let second = User::query()
        .join(User::POSTS)
        .order_by(Post::VIEWS.desc())
        .cursor_paginate(&db, first.next.as_ref(), 1)
        .await
        .unwrap();
    assert_eq!(second.items[0].name, "ann");
    assert!(second.next.is_none());

    // Expression orders now work without joins too.
    let page = Post::query()
        .order_by(Projection::<Post>::raw(r#"-"posts"."views""#, Vec::<i64>::new()).asc())
        .cursor_paginate(&db, None, 2)
        .await
        .unwrap();
    assert_eq!(
        page.items.iter().map(|p| p.views).collect::<Vec<_>>(),
        [500, 100]
    );
    let rest = Post::query()
        .order_by(Projection::<Post>::raw(r#"-"posts"."views""#, Vec::<i64>::new()).asc())
        .cursor_paginate(&db, page.next.as_ref(), 10)
        .await
        .unwrap();
    assert_eq!(
        rest.items.iter().map(|p| p.views).collect::<Vec<_>>(),
        [50, 5]
    );
}

#[rok_db::test]
async fn joined_bulk_writes(db: Db) {
    seed(&db).await;
    let n = Post::query()
        .join(Post::AUTHOR)
        .filter(User::ROLE.eq("admin"))
        .update()
        .set(Post::VIEWS, 0)
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(Post::filter(Post::VIEWS.eq(0)).count(&db).await.unwrap(), 2);

    // Soft delete, then restore through a join.
    let n = Post::query()
        .join(Post::AUTHOR)
        .filter(User::NAME.eq("bob"))
        .delete(&db)
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(Post::query().count(&db).await.unwrap(), 2);
    let n = Post::query()
        .join(Post::AUTHOR)
        .filter(User::NAME.eq("bob"))
        .with_trashed()
        .restore(&db)
        .await
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(Post::query().count(&db).await.unwrap(), 4);

    // A multiplying join updates each root row once.
    let n = User::query()
        .join(User::POSTS)
        .filter(Post::VIEWS.gt(0))
        .filter(User::NAME.eq("bob"))
        .update()
        .set(User::ROLE, "star")
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);
}

#[rok_db::test]
async fn joined_memoize(db: Db) {
    seed(&db).await;
    let db = Db::builder()
        .query_cache(16)
        .build_with_pool(db.pool().clone());
    let cache = db.cache().unwrap().clone();
    let ttl = std::time::Duration::from_secs(60);
    let admin_posts = || {
        Post::query()
            .join(Post::AUTHOR)
            .filter(User::ROLE.eq("admin"))
            .memoize(ttl)
    };
    assert_eq!(admin_posts().count(&db).await.unwrap(), 2);
    assert_eq!(admin_posts().count(&db).await.unwrap(), 2);
    assert_eq!(cache.stats().hits, 1);

    // Writing to the *joined* table invalidates the entry.
    User::filter(User::NAME.eq("bob"))
        .update()
        .set(User::ROLE, "admin")
        .exec(&db)
        .await
        .unwrap();
    assert_eq!(admin_posts().count(&db).await.unwrap(), 4);
}
