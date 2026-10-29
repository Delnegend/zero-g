import { afterEach, test } from "node:test";
import assert from "node:assert/strict";
import { getSettings, updateSettings } from "./settings";

const originalFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = originalFetch;
});

test("rejects invalid GET and PUT responses", async () => {
  globalThis.fetch = (async () =>
    Response.json({ log_retention_days: "invalid" })) as typeof fetch;
  const signal = new AbortController().signal;
  await assert.rejects(getSettings(signal), /Invalid settings response/);
  await assert.rejects(updateSettings({}, signal), /Invalid settings response/);
});

test("surfaces HTTP failures", async () => {
  globalThis.fetch = (async () =>
    new Response("Invalid settings", { status: 400 })) as typeof fetch;
  const signal = new AbortController().signal;
  await assert.rejects(getSettings(signal), /Invalid settings/);
  await assert.rejects(updateSettings({}, signal), /Invalid settings/);
});
