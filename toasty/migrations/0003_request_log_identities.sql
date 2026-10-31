ALTER TABLE "request_logs" ADD COLUMN "virtual_key_name" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "request_logs" ADD COLUMN "virtual_key_id" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "request_logs" ADD COLUMN "credential_name" TEXT;
-- #[toasty::breakpoint]
ALTER TABLE "request_logs" ADD COLUMN "credential_id" TEXT;
