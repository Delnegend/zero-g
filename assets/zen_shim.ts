// Minimal Bun TLS proxy for OpenCode Zen free tier.
// zero-g sends an already-attested request here over HTTP; this shim forwards
// it to opencode.ai using Bun's TLS stack (required to pass the gate).
const UPSTREAM =
  process.env.ZEN_UPSTREAM ?? "https://opencode.ai/inference/openai/v1";
const PORT = Number(process.env.PORT ?? 8090);

Bun.serve({
  port: PORT,
  idleTimeout: 120,
  async fetch(req) {
    const url = new URL(req.url);
    if (url.pathname === "/health" || url.pathname === "/v1/health") {
      return new Response("ok");
    }
    const target = `${UPSTREAM}${url.pathname.replace(/^\/v1/, "")}${url.search}`;
    const headers = new Headers(req.headers);
    headers.delete("host");
    const init: RequestInit = { method: req.method, headers };
    if (req.method !== "GET" && req.method !== "HEAD") init.body = req.body;
    const up = await fetch(target, init);
    return new Response(up.body, { status: up.status, headers: up.headers });
  },
});
console.log(`zen-shim on :${PORT} -> ${UPSTREAM}`);
