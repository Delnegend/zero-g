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

test("aborting interrupts the request", async () => {
  let markStarted!: () => void;
  const requestStarted = new Promise<void>((resolve) => {
    markStarted = resolve;
  });
  let requestSignal: AbortSignal | undefined;
  globalThis.fetch = ((_url: string | URL | Request, options?: RequestInit) => {
    requestSignal = options?.signal ?? undefined;
    markStarted();
    return new Promise<Response>((_resolve, reject) => {
      requestSignal?.addEventListener(
        "abort",
        () => reject(new DOMException("Aborted", "AbortError")),
        { once: true },
      );
    });
  }) as typeof fetch;
  const controller = new AbortController();
  const pending = getSettings(controller.signal);
  await requestStarted;
  controller.abort();
  await assert.rejects(pending);
  assert.equal(requestSignal?.aborted, true);
});
