import { Database } from "bun:sqlite";
import { createDecipheriv } from "node:crypto";
import { open, unlink } from "node:fs/promises";

const [output, ...extra] = process.argv.slice(2);
if (!output || extra.length) {
  throw new Error(
    "Usage: bun run scripts/export-credentials.ts <new-output-file>",
  );
}

const encodedKey = process.env.ZERO_G_ENCRYPTION_KEY?.trim();
if (!encodedKey || !/^[A-Za-z0-9+/]{43}=$/.test(encodedKey)) {
  throw new Error("ZERO_G_ENCRYPTION_KEY must be base64-encoded 32 bytes");
}
const key = Buffer.from(encodedKey, "base64");
if (key.length !== 32 || key.toString("base64") !== encodedKey) {
  throw new Error("ZERO_G_ENCRYPTION_KEY must be base64-encoded 32 bytes");
}

const path = process.env.ZERO_G_LOG_DB ?? "zero-g-logs.db";
if (!path || path === ":memory:") {
  throw new Error("ZERO_G_LOG_DB must be a file-backed database");
}

function decrypt(name: string, data: Uint8Array): string {
  if (data.length < 28) throw new Error("Invalid encrypted secret");
  try {
    const decipher = createDecipheriv("aes-256-gcm", key, data.subarray(0, 12));
    decipher.setAAD(Buffer.from(name));
    decipher.setAuthTag(data.subarray(-16));
    return Buffer.concat([
      decipher.update(data.subarray(12, -16)),
      decipher.final(),
    ]).toString("utf8");
  } catch {
    throw new Error("Unable to decrypt secret (wrong key or corrupted data)");
  }
}

type Secret = { name: string; value: Uint8Array };
type CredentialSecret = Secret & {
  provider_id: string;
  provider_name: string;
  provider_kind: string;
  credential_id: string;
  credential_name: string;
};

const db = new Database(path, { readonly: true, strict: true });
let secrets: CredentialSecret[];
try {
  db.exec("BEGIN");
  const sentinel = db
    .query<Secret, []>(
      "SELECT name, value FROM secrets WHERE name = '__zero_g_encryption_key_check'",
    )
    .get();
  if (
    !sentinel ||
    decrypt(sentinel.name, sentinel.value) !== "zero-g encryption key check v1"
  ) {
    throw new Error("Unable to verify the database encryption key");
  }
  secrets = db
    .query<CredentialSecret, []>(
      `
      SELECT p.id AS provider_id, p.name AS provider_name, p.kind AS provider_kind,
             c.id AS credential_id, c.name AS credential_name, s.name, s.value
      FROM provider_credentials AS c
      JOIN providers AS p ON p.id = c.provider_id
      JOIN secrets AS s ON substr(s.name, 1, length('credential.' || c.id || '.')) = 'credential.' || c.id || '.'
      WHERE p.deleted = 0
      ORDER BY p.id, c.id, s.name
    `,
    )
    .all();
  db.exec("COMMIT");
} finally {
  db.close();
}

const credentials = secrets.map(({ name, value, ...metadata }) => ({
  ...metadata,
  secret_type: name.slice(`credential.${metadata.credential_id}.`.length),
  value: decrypt(name, value),
}));

// Exclusive creation prevents overwriting an existing file or following a symlink.
const file = await open(output, "wx", 0o600);
try {
  await file.writeFile(`${JSON.stringify(credentials, null, 2)}\n`, "utf8");
} catch (error) {
  await file.close();
  await unlink(output);
  throw error;
}
await file.close();
console.log(`Exported ${credentials.length} credential secrets to ${output}`);
