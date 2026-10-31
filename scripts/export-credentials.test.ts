import { test } from "node:test";
import assert from "node:assert/strict";
import { Database } from "bun:sqlite";
import { createCipheriv, randomBytes } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

function seal(key: Buffer, name: string, value: string): Buffer {
  const nonce = randomBytes(12);
  const cipher = createCipheriv("aes-256-gcm", key, nonce);
  cipher.setAAD(Buffer.from(name));
  const encrypted = Buffer.concat([
    cipher.update(value, "utf8"),
    cipher.final(),
  ]);
  return Buffer.concat([nonce, encrypted, cipher.getAuthTag()]);
}

test("exports provider secrets and rejects wrong keys, corruption, and existing files", () => {
  const dir = mkdtempSync(join(tmpdir(), "zero-g-export-test-"));
  const dbPath = join(dir, "test.db");
  const output = join(dir, "export.json");
  const key = Buffer.alloc(32, 7);
  const db = new Database(dbPath);
  try {
    db.exec(`
      CREATE TABLE secrets (name TEXT PRIMARY KEY, value BLOB NOT NULL);
      CREATE TABLE providers (id TEXT PRIMARY KEY, name TEXT, kind TEXT, deleted INTEGER);
      CREATE TABLE provider_credentials (id TEXT PRIMARY KEY, provider_id TEXT, name TEXT);
      INSERT INTO providers VALUES ('example', 'Example', 'openai', 0);
      INSERT INTO provider_credentials VALUES ('one', 'example', 'Account');
    `);
    const insert = db.prepare("INSERT INTO secrets VALUES (?, ?)");
    for (const [name, value] of [
      ["__zero_g_encryption_key_check", "zero-g encryption key check v1"],
      ["credential.one.api_key", "test-api-key"],
      ["credential.one.credential", '{"refresh_token":"test-refresh-token"}'],
      ["credential.one.console_session", "test-console-session"],
    ]) {
      insert.run(name, seal(key, name, value));
    }
    db.close();
    const run = (keyValue: string, target: string) =>
      Bun.spawnSync(
        [process.execPath, "run", "scripts/export-credentials.ts", target],
        {
          cwd: process.cwd(),
          env: {
            ...process.env,
            ZERO_G_LOG_DB: dbPath,
            ZERO_G_ENCRYPTION_KEY: keyValue,
          },
          stdout: "pipe",
          stderr: "pipe",
        },
      );

    const badOutput = join(dir, "wrong.json");
    assert.notEqual(
      run(Buffer.alloc(32, 8).toString("base64"), badOutput).exitCode,
      0,
    );
    assert.throws(() => statSync(badOutput));
    const result = run(key.toString("base64"), output);
    assert.equal(result.exitCode, 0, result.stderr.toString());
    assert.doesNotMatch(
      result.stdout.toString(),
      /test-api-key|test-refresh-token|test-console-session/,
    );
    assert.doesNotMatch(
      result.stderr.toString(),
      /test-api-key|test-refresh-token|test-console-session/,
    );
    assert.equal(statSync(output).mode & 0o777, 0o600);
    const exported = JSON.parse(readFileSync(output, "utf8"));
    assert.deepEqual(
      exported.map((row: { secret_type: string; value: string }) => [
        row.secret_type,
        row.value,
      ]),
      [
        ["api_key", "test-api-key"],
        ["console_session", "test-console-session"],
        ["credential", '{"refresh_token":"test-refresh-token"}'],
      ],
    );
    assert.ok(
      exported.every(
        (row: { provider_id: string; credential_name: string }) =>
          row.provider_id === "example" && row.credential_name === "Account",
      ),
    );
    assert.notEqual(run(key.toString("base64"), output).exitCode, 0);
    assert.deepEqual(JSON.parse(readFileSync(output, "utf8")), exported);

    const corruptDb = new Database(dbPath);
    corruptDb
      .prepare("UPDATE secrets SET value = ? WHERE name = ?")
      .run(
        seal(Buffer.alloc(32, 9), "credential.one.api_key", "bad"),
        "credential.one.api_key",
      );
    corruptDb.close();
    const corruptOutput = join(dir, "corrupt.json");
    assert.notEqual(run(key.toString("base64"), corruptOutput).exitCode, 0);
    assert.throws(() => statSync(corruptOutput));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
