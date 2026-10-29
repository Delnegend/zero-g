import { stat } from "node:fs/promises";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { connect } from "@tursodatabase/database";

const path = process.env.ZERO_G_LOG_DB ?? "zero-g-logs.db";
if (
  path === ":memory:" ||
  !(await stat(path).then(
    (file) => file.isFile(),
    () => false,
  ))
) {
  throw new Error(
    `Database not found: ${path}. Start zero-g once to initialize it, then stop it before seeding.`,
  );
}

const providers = [
  {
    name: "codex",
    label: "OpenAI Codex",
    kind: "codex",
    baseUrl: "",
    model: "gpt-5.3-codex",
    path: "/codex/codex/v1/responses",
  },
  {
    name: "agy",
    label: "Google Antigravity",
    kind: "agy",
    baseUrl: "",
    model: "gemini-3-pro",
    path: "/agy/agy/v1/chat/completions",
  },
  {
    name: "zen",
    label: "OpenCode Zen",
    kind: "zen",
    baseUrl: "http://127.0.0.1:8090/v1",
    model: "big-pickle",
    path: "/zen/zen/v1/chat/completions",
  },
] as const;
const now = Math.floor(Date.now() / 1000);
const count = 140;
const keys = [
  { name: "Example Codex key", provider: providers[0] },
  { name: "Example Zen key", provider: providers[2] },
].map((key) => ({
  ...key,
  id: randomUUID(),
  token: `zg-${randomBytes(32).toString("base64url")}`,
}));

// Turso's default file locking requires the server to be stopped first.
const db = await connect(path);
try {
  const seed = db.transactionAsync(async (tx) => {
    const providerIds = new Map<string, string>();
    const existingProvider = await tx.prepare(
      "SELECT id FROM providers WHERE kind = ? AND deleted = false ORDER BY id LIMIT 1",
    );
    const addProvider = await tx.prepare(
      "INSERT INTO providers (id, name, kind, base_url, deleted) VALUES (?, ?, ?, ?, false) ON CONFLICT(id) DO NOTHING",
    );
    const checkProvider = await tx.prepare(
      "SELECT id FROM providers WHERE id = ? AND name = ? AND kind = ? AND base_url = ? AND deleted = false",
    );
    const addKey = await tx.prepare(
      "INSERT INTO virtual_keys (id, name, key_hash, providers, models, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    );
    try {
      for (const provider of providers) {
        const existing = (await existingProvider.get(provider.kind)) as
          { id: string } | undefined;
        if (existing) {
          providerIds.set(provider.name, existing.id);
          continue;
        }
        await addProvider.run(
          provider.name,
          provider.label,
          provider.kind,
          provider.baseUrl,
        );
        if (
          !(await checkProvider.get(
            provider.name,
            provider.label,
            provider.kind,
            provider.baseUrl,
          ))
        ) {
          throw new Error(
            `Provider ID ${provider.name} is already in use or deleted; no seed data was added.`,
          );
        }
        providerIds.set(provider.name, provider.name);
      }
      for (const key of keys) {
        await addKey.run(
          key.id,
          key.name,
          createHash("sha256").update(key.token).digest(),
          JSON.stringify([providerIds.get(key.provider.name)]),
          JSON.stringify([key.provider.model]),
          now,
        );
      }
    } finally {
      await addProvider.close();
      await checkProvider.close();
      await existingProvider.close();
      await addKey.close();
    }
    const insert = await tx.prepare(`INSERT INTO request_logs
      (created_at, provider, model, path, status, latency_ms, input_tokens, output_tokens, request_body, response_body)
      VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
    try {
      for (let i = 0; i < count; i++) {
        const provider = providers[i % providers.length];
        const status = i % 19 === 0 ? 500 : i % 13 === 0 ? 429 : 200;
        const input = 120 + ((i * 97) % 4000);
        const output = 40 + ((i * 53) % 1200);
        await insert.run(
          now - i * 3600,
          providerIds.get(provider.name),
          provider.model,
          provider.path.replace(
            `/${provider.name}/`,
            `/${providerIds.get(provider.name)}/`,
          ),
          status,
          180 + ((i * 137) % 4500),
          status === 200 ? input : null,
          status === 200 ? output : null,
          JSON.stringify({
            _seed: true,
            model: provider.model,
            messages: [{ role: "user", content: `Example request ${i + 1}` }],
          }),
          JSON.stringify(
            status === 200
              ? {
                  _seed: true,
                  choices: [
                    {
                      message: {
                        role: "assistant",
                        content: "Example response for dashboard development.",
                      },
                    },
                  ],
                  usage: { prompt_tokens: input, completion_tokens: output },
                }
              : {
                  _seed: true,
                  error: {
                    message:
                      status === 429
                        ? "Example rate limit"
                        : "Example provider error",
                  },
                },
          ),
        );
      }
    } finally {
      await insert.close();
    }
  });
  await seed();
  console.log(
    `Added ${count} example request logs and ${keys.length} virtual keys to ${path}.`,
  );
  for (const key of keys) console.log(`${key.name}: ${key.token}`);
  console.log(
    "Copy these keys now; they cannot be retrieved later. Providers require separate upstream credentials.",
  );
} finally {
  await db.close();
}
