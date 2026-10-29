CREATE TABLE "provider_credentials" (
    "id" TEXT NOT NULL,
    "provider_id" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "weight" BIGINT NOT NULL,
    "enabled" BOOLEAN NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_provider_credentials_by_provider_id" ON "provider_credentials" ("provider_id");
