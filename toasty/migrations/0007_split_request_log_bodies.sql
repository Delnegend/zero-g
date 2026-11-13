CREATE TABLE "request_log_bodies" (
    "log_id" BIGINT NOT NULL,
    "created_at" BIGINT NOT NULL,
    "request_body" TEXT NOT NULL,
    "response_body" TEXT NOT NULL,
    PRIMARY KEY ("log_id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_request_log_bodies_by_created_at" ON "request_log_bodies" ("created_at");
-- #[toasty::breakpoint]
INSERT INTO "request_log_bodies" ("log_id", "created_at", "request_body", "response_body")
SELECT "id", "created_at", "request_body", "response_body" FROM "request_logs";
-- #[toasty::breakpoint]
ALTER TABLE "request_logs" DROP COLUMN "request_body";
-- #[toasty::breakpoint]
ALTER TABLE "request_logs" DROP COLUMN "response_body";
