-- A person who can sign in.
CREATE TABLE users (
    id          BIGSERIAL PRIMARY KEY,
    -- Unique, compared case-insensitively by the queries below.
    email       TEXT NOT NULL UNIQUE,
    name        TEXT,
    role        user_role NOT NULL DEFAULT 'member',
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at  TIMESTAMPTZ
);

CREATE INDEX users_role_idx ON users (role);

-- name: find_by_email :one
SELECT * FROM users WHERE lower(email) = lower($1) AND deleted_at IS NULL;

-- How many active users have each role.
-- name: count_by_role :many
SELECT role, count(*) AS "total!"
FROM users
WHERE deleted_at IS NULL
GROUP BY role
ORDER BY role;

-- name: emails :many
SELECT email FROM users WHERE deleted_at IS NULL ORDER BY email;

-- name: promote :exec
UPDATE users SET role = 'admin', updated_at = now() WHERE id = $1;
