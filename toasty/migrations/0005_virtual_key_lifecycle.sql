ALTER TABLE "virtual_keys" ADD COLUMN "expires_at" INTEGER;
-- #[toasty::breakpoint]
ALTER TABLE "virtual_keys" ADD COLUMN "revoked_at" INTEGER;
