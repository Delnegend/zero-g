import { stat } from "node:fs/promises";
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
  { name: "codex", model: "gpt-5.3-codex", path: "/codex/v1/responses" },
  { name: "agy", model: "gemini-3-pro", path: "/agy/v1/chat/completions" },
  { name: "zen", model: "big-pickle", path: "/zen/v1/chat/completions" },
] as const;
const now = Math.floor(Date.now() / 1000);
const count = 140;

// Turso's default file locking requires the server to be stopped first.
const db = await connect(path);
try {
  const seed = db.transactionAsync(async (tx) => {
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
          provider.name,
          provider.model,
          provider.path,
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
  console.log(`Added ${count} example request logs to ${path}.`);
} finally {
  await db.close();
}
