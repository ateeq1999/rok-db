DROP INDEX IF EXISTS "users_role_idx";
DROP INDEX IF EXISTS "posts_author_idx";
ALTER TABLE "posts" DROP CONSTRAINT "posts_author_id_fkey";
DROP TABLE "users";
DROP TABLE "posts";
DROP TYPE "user_role";
