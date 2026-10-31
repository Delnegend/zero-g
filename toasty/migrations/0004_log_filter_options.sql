CREATE TABLE "log_filter_options" (
    "id" TEXT NOT NULL,
    "kind" BIGINT NOT NULL,
    "value" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "last_seen" BIGINT NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE INDEX "index_log_filter_options_by_last_seen" ON "log_filter_options" ("last_seen");
-- #[toasty::breakpoint]
INSERT INTO "log_filter_options" ("id", "kind", "value", "name", "last_seen")
SELECT kind || ':' || length(CAST(value AS BLOB)) || ':' || value || name, kind, value, name, last_seen FROM (
    SELECT 0 AS kind, provider AS value, '' AS name, MAX(created_at) AS last_seen FROM request_logs WHERE provider <> '' GROUP BY provider
    UNION ALL SELECT 1, model, '', MAX(created_at) FROM request_logs WHERE model <> '' GROUP BY model
    UNION ALL SELECT 2, path, '', MAX(created_at) FROM request_logs WHERE path <> '' GROUP BY path
    UNION ALL SELECT 3, virtual_key_id, virtual_key_name, MAX(created_at) FROM request_logs WHERE virtual_key_id IS NOT NULL AND virtual_key_name IS NOT NULL GROUP BY virtual_key_id, virtual_key_name
    UNION ALL SELECT 4, credential_id, credential_name, MAX(created_at) FROM request_logs WHERE credential_id IS NOT NULL AND credential_name IS NOT NULL GROUP BY credential_id, credential_name
);
