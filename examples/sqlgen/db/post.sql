-- Something a user wrote.
CREATE TABLE posts (
    id            BIGSERIAL PRIMARY KEY,
    author_id     BIGINT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title         TEXT NOT NULL,
    body          TEXT NOT NULL DEFAULT '',
    tags          TEXT[] NOT NULL DEFAULT '{}',
    views         INT NOT NULL DEFAULT 0,
    published_at  TIMESTAMPTZ
);

CREATE INDEX posts_author_idx ON posts (author_id);

-- name: create_post :one!
INSERT INTO posts (author_id, title, tags) VALUES ($1, $2, $3) RETURNING *;

-- name: top_for_author :many
SELECT * FROM posts WHERE author_id = $1 ORDER BY views DESC, id LIMIT $2;

-- Titles with their author's email, newest first.
-- name: feed :stream
SELECT p.id, p.title, u.email AS author_email
FROM posts p
JOIN users u ON u.id = p.author_id
WHERE p.published_at IS NOT NULL AND u.deleted_at IS NULL
ORDER BY p.published_at DESC;

-- name: add_view :exec
UPDATE posts SET views = views + 1 WHERE id = $1;
