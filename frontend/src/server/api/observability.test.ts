import { afterEach, test } from "node:test";
import assert from "node:assert/strict";
import { QueryClient } from "@tanstack/react-query";
import { observabilityQueryOptions, rangeStart } from "./observability";

const originalFetch = globalThis.fetch;
const originalNow = Date.now;

afterEach(() => {
  globalThis.fetch = originalFetch;
  Date.now = originalNow;
});

const response = {
  breakdown: { models: [], providers: [] },
  buckets: [],
  filter_options: {
    credentials: [],
    models: [],
    paths: [],
    providers: [],
    virtual_keys: [],
  },
  interval: "hour",
  logs: [],
  retention_cutoff: -1000000000,
  summary: {
    avg_latency_ms: 0,
    errors: 0,
    input_tokens: null,
    output_tokens: null,
    requests: 0,
    successes: 0,
  },
};

test("dashboard sends adjacent inclusive ranges with identical filters and refreshes both", async () => {
  const calls: URL[] = [];
  globalThis.fetch = (async (url) => {
    calls.push(new URL(String(url), "http://localhost"));
    return Response.json(response);
  }) as typeof fetch;
  let now = Date.parse("2024-03-31T12:00:00Z");
  Date.now = () => now;
  const filters = {
    providers: ["openai"],
    models: ["model"],
    paths: ["/chat"],
    statuses: ["success", "error"] as ("success" | "error")[],
    virtual_key_ids: ["key"],
    credential_ids: ["credential"],
  };
  const client = new QueryClient();
  const options = observabilityQueryOptions(
    { kind: "preset", value: "1m" },
    false,
    filters,
  );
  const result = await client.fetchQuery(options);
  const currentSince = Date.parse("2024-02-29T12:00:00Z") / 1000;
  const currentUntil = now / 1000;
  const previousUntil = currentSince - 1;
  const previousSince = previousUntil - (currentUntil - currentSince);
  assert.deepEqual(result.comparison, {
    data: response,
    currentSince,
    currentUntil,
    previousSince,
    previousUntil,
    retentionCutoff: response.retention_cutoff,
  });
  assert.deepEqual(
    calls.map((url) => [
      Number(url.searchParams.get("since")),
      Number(url.searchParams.get("until")),
    ]),
    [
      [currentSince, currentUntil],
      [previousSince, previousUntil],
    ],
  );
  for (const call of calls) {
    for (const [key, value] of Object.entries(filters)) {
      assert.deepEqual(JSON.parse(call.searchParams.get(key)!), value);
    }
  }
  now += 1000;
  await client.fetchQuery(options);
  assert.equal(calls.length, 4);
  assert.equal(Number(calls[2].searchParams.get("until")), currentUntil + 1);
  assert.equal(Number(calls[3].searchParams.get("until")), currentSince);
  client.clear();
});

test("custom and single-second ranges preserve inclusive boundaries", async () => {
  Date.now = () => 1000 * 1000;
  const calls: URL[] = [];
  globalThis.fetch = (async (url) => {
    calls.push(new URL(String(url), "http://localhost"));
    return Response.json(response);
  }) as typeof fetch;
  const client = new QueryClient();
  const data = await client.fetchQuery(
    observabilityQueryOptions({ kind: "custom", from: 100, until: 100 }, false),
  );
  assert.deepEqual(data.comparison, {
    data: response,
    currentSince: 100,
    currentUntil: 100,
    previousSince: 99,
    previousUntil: 99,
    retentionCutoff: response.retention_cutoff,
  });
  assert.deepEqual(
    calls.map((url) => [
      url.searchParams.get("since"),
      url.searchParams.get("until"),
    ]),
    [
      ["100", "100"],
      ["99", "99"],
    ],
  );
  client.clear();
});

test("calendar presets clamp month ends and leap days", () => {
  assert.equal(
    rangeStart("1m", Date.parse("2023-03-31T08:00:00Z")),
    Date.parse("2023-02-28T08:00:00Z") / 1000,
  );
  assert.equal(
    rangeStart("6m", Date.parse("2024-08-31T08:00:00Z")),
    Date.parse("2024-02-29T08:00:00Z") / 1000,
  );
  assert.equal(
    rangeStart("1y", Date.parse("2024-02-29T08:00:00Z")),
    Date.parse("2023-02-28T08:00:00Z") / 1000,
  );
});

test("previous failure retains current data while current failure rejects", async () => {
  Date.now = () => 1000 * 1000;
  let failedSince = "89";
  globalThis.fetch = (async (url) => {
    const since = new URL(String(url), "http://localhost").searchParams.get(
      "since",
    );
    return since === failedSince
      ? new Response(null, { status: 503 })
      : Response.json(response);
  }) as typeof fetch;
  const client = new QueryClient();
  const options = observabilityQueryOptions(
    { kind: "custom", from: 100, until: 110 },
    false,
  );
  const data = await client.fetchQuery(options);
  assert.deepEqual(data.summary, response.summary);
  assert.equal(data.comparison?.data, null);
  failedSince = "100";
  await assert.rejects(client.fetchQuery(options), /503/);
  client.clear();
});

test("active retention cutoff controls comparisons without requesting pending settings", async () => {
  const calls: string[] = [];
  globalThis.fetch = (async (url) => {
    calls.push(String(url));
    return Response.json({ ...response, retention_cutoff: 13600 });
  }) as typeof fetch;
  const client = new QueryClient();
  const partial = await client.fetchQuery(
    observabilityQueryOptions(
      { kind: "custom", from: 13601, until: 13602 },
      false,
    ),
  );
  assert.equal(partial.comparison?.retentionCutoff, 13600);
  assert.equal(partial.comparison?.data, null);
  const complete = await client.fetchQuery(
    observabilityQueryOptions(
      { kind: "custom", from: 13602, until: 13603 },
      false,
    ),
  );
  assert.deepEqual(complete.comparison?.data, {
    ...response,
    retention_cutoff: 13600,
  });
  assert.equal(calls.length, 4);
  assert.ok(calls.every((url) => url.startsWith("/api/observability?")));
  client.clear();
});

test("logs-only queries issue one unbounded request and no comparison", async () => {
  const calls: URL[] = [];
  globalThis.fetch = (async (url) => {
    calls.push(new URL(String(url), "http://localhost"));
    return Response.json(response);
  }) as typeof fetch;
  const client = new QueryClient();
  const result = await client.fetchQuery(
    observabilityQueryOptions({ kind: "custom", from: 100, until: 110 }, true, {
      models: ["model"],
    }),
  );
  assert.deepEqual(result, response);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].searchParams.get("logs_only"), "true");
  assert.equal(calls[0].searchParams.get("since"), null);
  assert.equal(calls[0].searchParams.get("until"), null);
  assert.deepEqual(JSON.parse(calls[0].searchParams.get("models")!), ["model"]);
  client.clear();
});
