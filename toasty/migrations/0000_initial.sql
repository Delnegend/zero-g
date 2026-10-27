CREATE TABLE "secrets" (
    "name" TEXT NOT NULL,
    "value" BLOB NOT NULL,
    PRIMARY KEY ("name")
);
-- #[toasty::breakpoint]
CREATE TABLE "settings" (
    "name" TEXT NOT NULL,
    "value" TEXT NOT NULL,
    PRIMARY KEY ("name")
);
-- #[toasty::breakpoint]
CREATE TABLE "virtual_keys" (
    "id" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "key_hash" BLOB NOT NULL,
    "providers" TEXT NOT NULL,
    "models" TEXT NOT NULL,
    "created_at" BIGINT NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_virtual_keys_by_key_hash" ON "virtual_keys" ("key_hash");
-- #[toasty::breakpoint]
CREATE TABLE "request_logs" (
    "id" INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    "created_at" BIGINT NOT NULL,
    "provider" TEXT NOT NULL,
    "model" TEXT NOT NULL,
    "path" TEXT NOT NULL,
    "status" BIGINT NOT NULL,
    "latency_ms" BIGINT NOT NULL,
    "input_tokens" BIGINT,
    "output_tokens" BIGINT,
    "request_body" TEXT NOT NULL,
    "response_body" TEXT NOT NULL
);
-- #[toasty::breakpoint]
CREATE INDEX "index_request_logs_by_created_at" ON "request_logs" ("created_at");
