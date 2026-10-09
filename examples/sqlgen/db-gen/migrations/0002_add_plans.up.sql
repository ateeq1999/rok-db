CREATE TABLE "plans" (
    "id" BIGSERIAL NOT NULL,
    "name" TEXT NOT NULL,
    "price" NUMERIC(10,2) NOT NULL,
    "period" INTERVAL NOT NULL DEFAULT '1 month',
    "trial" INTERVAL,
    CONSTRAINT "plans_pkey" PRIMARY KEY ("id"),
    CONSTRAINT "plans_name_key" UNIQUE ("name")
);
