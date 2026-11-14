-- Bodies move from "request_log_bodies" (plain text) to zstd-compressed
-- "request_log_payloads". The old table is left in place on purpose: startup
-- compresses its rows in batches and drops it once empty, because SQL cannot
-- compress and dropping it here would lose every stored body.
CREATE TABLE "request_log_payloads" (
    "log_id" BIGINT NOT NULL,
    "created_at" BIGINT NOT NULL,
    "request_body" BLOB NOT NULL,
    "response_body" BLOB NOT NULL,
    PRIMARY KEY ("log_id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_request_log_payloads_by_created_at" ON "request_log_payloads" ("created_at");
