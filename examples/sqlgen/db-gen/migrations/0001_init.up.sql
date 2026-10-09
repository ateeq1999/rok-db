CREATE TYPE "user_role" AS ENUM ('admin', 'editor', 'member');
CREATE TABLE "posts" (
    "id" BIGSERIAL NOT NULL,
    "author_id" BIGINT NOT NULL,
    "title" TEXT NOT NULL,
    "body" TEXT NOT NULL DEFAULT '',
    "tags" TEXT[] NOT NULL DEFAULT '{}',
    "views" INTEGER NOT NULL DEFAULT 0,
    "published_at" TIMESTAMPTZ,
    CONSTRAINT "posts_pkey" PRIMARY KEY ("id")
);
CREATE TABLE "users" (
    "id" BIGSERIAL NOT NULL,
    "email" TEXT NOT NULL,
    "name" TEXT,
    "role" "user_role" NOT NULL DEFAULT 'member',
    "created_at" TIMESTAMPTZ NOT NULL DEFAULT now(),
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT now(),
    "deleted_at" TIMESTAMPTZ,
    CONSTRAINT "users_pkey" PRIMARY KEY ("id"),
    CONSTRAINT "users_email_key" UNIQUE ("email")
);
ALTER TABLE "posts" ADD CONSTRAINT "posts_author_id_fkey" FOREIGN KEY ("author_id") REFERENCES "users" ("id") ON DELETE CASCADE;
CREATE INDEX posts_author_idx ON posts(author_id);
CREATE INDEX users_role_idx ON users(role);
