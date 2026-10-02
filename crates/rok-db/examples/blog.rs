//! A small tour of rok-db.
//!
//! ```sh
//! DATABASE_URL=postgres://postgres:postgres@localhost/app cargo run -p rok-db --example blog
//! ```

use rok_db::prelude::*;

#[derive(Debug, Clone, Model)]
struct Author {
    #[rok(primary_key, generated)]
    id: i64,
    name: String,
    email: String,
}

#[derive(Debug, Clone, Model)]
struct Post {
    #[rok(generated)]
    id: i64,
    author_id: i64,
    title: String,
    published: bool,
    #[rok(generated)]
    views: i32,
}

#[tokio::main]
async fn main() -> rok_db::Result<()> {
    let db = Db::builder().max_connections(5).connect_env().await?;

    db.execute(
        "DROP TABLE IF EXISTS posts, authors;
         CREATE TABLE authors (id BIGSERIAL PRIMARY KEY, name TEXT NOT NULL, email TEXT NOT NULL UNIQUE);
         CREATE TABLE posts (
             id BIGSERIAL PRIMARY KEY,
             author_id BIGINT NOT NULL REFERENCES authors (id) ON DELETE CASCADE,
             title TEXT NOT NULL,
             published BOOLEAN NOT NULL,
             views INT NOT NULL DEFAULT 0
         );",
    )
    .await?;

    // Insert an author and their posts atomically.
    let author = db
        .transaction(|tx| {
            Box::pin(async move {
                let author = Author::create()
                    .set(Author::NAME, "Ada")
                    .set(Author::EMAIL, "ada@example.com")
                    .exec(&mut *tx)
                    .await?;

                let drafts: Vec<Post> = (1..=12)
                    .map(|i| Post {
                        id: 0,
                        author_id: author.id,
                        title: format!("Notes #{i}"),
                        published: i % 3 != 0,
                        views: 0,
                    })
                    .collect();
                Post::insert_all(&mut *tx, &drafts).await?;
                Ok::<_, rok_db::Error>(author)
            })
        })
        .await?;
    println!("created {author:?}");

    // Bump view counters in bulk.
    Post::filter(Post::TITLE.contains("#1"))
        .update()
        .increment(Post::VIEWS, 100)
        .exec(&db)
        .await?;

    // Paginate published posts, most viewed first.
    let page = Post::filter(Post::PUBLISHED.eq(true))
        .filter(Post::AUTHOR_ID.eq(author.id))
        .order_by(Post::VIEWS.desc())
        .order_by(Post::ID)
        .paginate(&db, 1, 5)
        .await?;
    println!(
        "page {}/{} ({} published posts)",
        page.page,
        page.total_pages(),
        page.total
    );
    for post in &page {
        println!("  {:<10} {:>4} views", post.title, post.views);
    }

    // Raw SQL is always available.
    let total_views: i64 =
        rok_db::raw("SELECT COALESCE(SUM(views), 0)::BIGINT FROM posts WHERE author_id = ?")
            .bind(author.id)
            .scalar(&db)
            .await?;
    println!("total views: {total_views}");

    author.delete(&db).await?;
    println!("remaining posts after cascade: {}", Post::count(&db).await?);
    Ok(())
}
