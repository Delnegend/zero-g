import { afterEach, test } from "node:test";
import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});
for (const name of [
  "window",
  "document",
  "navigator",
  "HTMLElement",
  "HTMLInputElement",
  "HTMLButtonElement",
  "Element",
  "Node",
  "Document",
  "ShadowRoot",
  "NodeFilter",
  "MutationObserver",
  "getComputedStyle",
  "Event",
  "MouseEvent",
  "KeyboardEvent",
  "CustomEvent",
  "DOMException",
  "SVGElement",
  "DOMRect",
] as const) {
  Object.defineProperty(globalThis, name, {
    value: dom.window[name],
    configurable: true,
    writable: true,
  });
}
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
  value: true,
  configurable: true,
  writable: true,
});
Object.defineProperty(window, "matchMedia", {
  value: () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
  }),
});
class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
Object.defineProperty(globalThis, "ResizeObserver", {
  value: ResizeObserverStub,
  configurable: true,
});
Object.defineProperty(window, "ResizeObserver", {
  value: ResizeObserverStub,
  configurable: true,
});
Object.defineProperty(globalThis, "PointerEvent", {
  value: dom.window.MouseEvent,
  configurable: true,
});
Object.defineProperty(window, "PointerEvent", {
  value: dom.window.MouseEvent,
  configurable: true,
});
Object.defineProperty(dom.window.HTMLElement.prototype, "scrollIntoView", {
  value() {},
  configurable: true,
});
Object.defineProperty(globalThis, "requestAnimationFrame", {
  value: (callback: FrameRequestCallback) =>
    setTimeout(() => callback(Date.now()), 0),
  configurable: true,
});
Object.defineProperty(globalThis, "cancelAnimationFrame", {
  value: clearTimeout,
  configurable: true,
});

// These modules capture the browser document at import time, so load them after JSDOM setup.
const { render, screen, within, cleanup, waitFor } =
  await import("@testing-library/react");
const userEvent = (await import("@testing-library/user-event")).default;
const { Management } = await import("./Management");
const { ObservabilityPage } = await import("./App");
const { Credentials } = await import("./components/providers/Credentials");
const originalFetch = globalThis.fetch;

afterEach(() => {
  cleanup();
  globalThis.fetch = originalFetch;
});

function mount(element: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>{element}</QueryClientProvider>,
  );
}

const settings = {
  log_retention_days: 30,
  oauth_client_id: "original-client",
  oauth_client_secret_set: true,
};

test("settings retain a blank secret, and clear it only when requested", async () => {
  const bodies: unknown[] = [];
  let stored = { ...settings };
  globalThis.fetch = (async (_url, options) => {
    if (options?.method === "PUT") {
      const body = JSON.parse(String(options.body));
      bodies.push(body);
      stored = {
        ...stored,
        log_retention_days: body.log_retention_days,
        oauth_client_id: body.oauth_client_id,
        oauth_client_secret_set:
          body.oauth_client_secret === ""
            ? false
            : stored.oauth_client_secret_set,
      };
    }
    return Response.json(stored);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<Management page="settings" />);
  await screen.findByDisplayValue("original-client");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await screen.findByText("Settings saved.");
  assert.deepEqual(bodies[0], {
    log_retention_days: 30,
    oauth_client_id: "original-client",
  });
  await user.click(
    screen.getByRole("checkbox", { name: "Clear Google OAuth client secret" }),
  );
  assert.equal(
    (screen.getByLabelText(/^Google OAuth client secret/) as HTMLInputElement)
      .disabled,
    true,
  );
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => assert.equal(bodies.length, 2));
  assert.deepEqual(bodies[1], {
    log_retention_days: 30,
    oauth_client_id: "original-client",
    oauth_client_secret: "",
  });
  await screen.findByText(/not configured/);
});

test("edits made during a delayed settings save survive the response and can be saved", async () => {
  const bodies: unknown[] = [];
  let release!: (response: Response) => void;
  const delayed = new Promise<Response>((resolve) => {
    release = resolve;
  });
  globalThis.fetch = (async (_url, options) => {
    if (options?.method !== "PUT") return Response.json(settings);
    const body = JSON.parse(String(options.body));
    bodies.push(body);
    if (bodies.length === 1) return delayed;
    return Response.json({
      log_retention_days: 30,
      oauth_client_id: "later-client",
      oauth_client_secret_set: true,
    });
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<Management page="settings" />);
  const clientId = await screen.findByDisplayValue("original-client");
  await user.clear(clientId);
  await user.type(clientId, "first-client");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => assert.equal(bodies.length, 1));
  await user.clear(clientId);
  await user.type(clientId, "later-client");
  release(Response.json({ ...settings, oauth_client_id: "first-client" }));
  await screen.findByText("Settings saved.");
  assert.equal((clientId as HTMLInputElement).value, "later-client");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => assert.equal(bodies.length, 2));
  assert.deepEqual(bodies, [
    { log_retention_days: 30, oauth_client_id: "first-client" },
    { log_retention_days: 30, oauth_client_id: "later-client" },
  ]);
});

test("credential editing retains a blank API key unless keyless is selected", async () => {
  const requests: unknown[] = [];
  let secret = "existing-secret";
  let credential = {
    id: "credential-1",
    name: "Production",
    weight: 1,
    enabled: true,
    connected: true,
  };
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    if (
      path === "/api/providers/alpha/credentials" &&
      options?.method === "GET"
    ) {
      return Response.json({ credentials: [credential] });
    }
    if (
      path === "/api/providers/alpha/credentials/credential-1" &&
      options?.method === "PUT"
    ) {
      const body = JSON.parse(String(options.body));
      requests.push(body);
      if ("api_key" in body) secret = body.api_key;
      credential = {
        ...credential,
        name: body.name,
        weight: body.weight,
        enabled: body.enabled,
        connected: secret.length > 0,
      };
      return Response.json(credential);
    }
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <Credentials
      provider={{
        id: "alpha",
        name: "Alpha",
        kind: "openai",
        base_url: "https://example.test",
      }}
      kind={{
        value: "openai",
        label: "OpenAI",
        api_key: true,
        oauth: false,
        fixed_id: null,
        fixed_name: null,
      }}
      onLogin={() => {}}
      loginBusy={false}
    />,
  );
  await screen.findByText(/Weight: 1.*Enabled.*Connected/);
  await user.click(screen.getByRole("button", { name: "Edit" }));
  let dialog = await screen.findByRole("dialog");
  await user.clear(within(dialog).getByLabelText("Name"));
  await user.type(within(dialog).getByLabelText("Name"), "Renamed");
  await user.click(
    within(dialog).getByRole("button", { name: "Save credential" }),
  );
  await waitFor(() => assert.equal(screen.queryByRole("dialog"), null));
  assert.deepEqual(requests[0], { name: "Renamed", weight: 1, enabled: true });
  await screen.findByText("Renamed");
  await screen.findByText(/Weight: 1.*Enabled.*Connected/);
  await user.click(screen.getByRole("button", { name: "Edit" }));
  dialog = await screen.findByRole("dialog");
  await user.click(
    within(dialog).getByRole("checkbox", { name: "Use a keyless upstream" }),
  );
  assert.equal(
    (within(dialog).getByLabelText(/^API key/) as HTMLInputElement).disabled,
    true,
  );
  await user.click(
    within(dialog).getByRole("button", { name: "Save credential" }),
  );
  await screen.findByText(/Weight: 1.*Enabled.*Not connected/);
  assert.deepEqual(requests[1], {
    name: "Renamed",
    weight: 1,
    enabled: true,
    api_key: "",
  });
});

test("create a restricted key, dismiss its one-time token, and revoke it", async () => {
  const requests: unknown[] = [];
  let keys: Array<{
    id: string;
    name: string;
    providers: string[];
    models: string[];
    created_at: number;
  }> = [];
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    if (path === "/api/providers")
      return Response.json({
        providers: [
          {
            id: "alpha",
            name: "Alpha",
            kind: "openai",
            base_url: "https://example.test",
          },
          {
            id: "beta",
            name: "Beta",
            kind: "openai",
            base_url: "https://example.test",
          },
        ],
      });
    if (path === "/api/keys" && options?.method === "POST") {
      const body = JSON.parse(String(options.body));
      requests.push(body);
      if (
        body.name !== "CI key" ||
        JSON.stringify(body.providers) !== '["alpha"]' ||
        JSON.stringify(body.models) !== '["model-a","model-b"]'
      )
        return new Response("Bad restrictions", { status: 400 });
      const key = {
        id: "key-1",
        name: "CI key",
        providers: ["alpha"],
        models: ["model-a", "model-b"],
        created_at: 1,
      };
      keys = [key];
      return Response.json({ ...key, key: "secret-once" });
    }
    if (path === "/api/keys/key-1" && options?.method === "DELETE") {
      requests.push({ method: "DELETE", path });
      keys = [];
      return new Response(null, { status: 204 });
    }
    if (path === "/api/keys") return Response.json({ keys });
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<Management page="keys" />);
  await screen.findByText("No keys created yet.");
  await user.click(screen.getByRole("button", { name: "Create key" }));
  const dialog = await screen.findByRole("dialog");
  await user.type(within(dialog).getByLabelText("Name"), "CI key");
  await user.click(
    await within(dialog).findByRole("checkbox", { name: "Alpha (alpha)" }),
  );
  await user.type(within(dialog).getByLabelText(/Models/), "model-a, model-b");
  await user.click(within(dialog).getByRole("button", { name: "Create key" }));
  await screen.findByText("secret-once");
  assert.deepEqual(requests[0], {
    name: "CI key",
    providers: ["alpha"],
    models: ["model-a", "model-b"],
  });
  assert.ok(screen.getByText("It will not be shown again."));
  await user.click(screen.getByRole("button", { name: "Dismiss" }));
  assert.equal(screen.queryByText("secret-once"), null);
  await screen.findByText("CI key");
  assert.ok(screen.getByText(/Providers: alpha.*Models: model-a, model-b/));
  await user.click(screen.getByRole("button", { name: "Revoke / delete" }));
  const confirmation = await screen.findByRole("dialog");
  assert.ok(within(confirmation).getByText(/CI key.*cannot be undone/));
  await user.click(
    within(confirmation).getByRole("button", { name: "Revoke / delete" }),
  );
  await screen.findByText("No keys created yet.");
  assert.deepEqual(requests[1], { method: "DELETE", path: "/api/keys/key-1" });
  assert.equal(screen.queryByText("secret-once"), null);
});

test("inspect a log, close its details, and change the dashboard range", async () => {
  const ranges: Array<[number, number]> = [];
  const log = {
    id: 42,
    created_at: 1700000000,
    input_tokens: 12,
    output_tokens: 4,
    latency_ms: 123,
    model: "model-a",
    path: "/v1/chat/completions",
    provider: "alpha",
    request_body: '{"prompt":"hello"}',
    response_body: '{"answer":"world"}',
    status: 200,
  };
  globalThis.fetch = (async (url) => {
    const parsed = new URL(String(url), "http://localhost");
    assert.equal(parsed.pathname, "/api/observability");
    ranges.push([
      Number(parsed.searchParams.get("since")),
      Number(parsed.searchParams.get("until")),
    ]);
    return Response.json({
      interval: "hour",
      buckets: [],
      logs: [log],
      summary: {
        avg_latency_ms: 123,
        errors: 0,
        input_tokens: 12,
        output_tokens: 4,
        requests: 1,
        successes: 1,
      },
    });
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<ObservabilityPage />);
  await user.click(
    (await screen.findAllByRole("button", { name: "View request 42" }))[0],
  );
  const details = await screen.findByRole("dialog");
  assert.ok(within(details).getByText(/alpha \/ model-a/));
  assert.ok(within(details).getByText("123 ms"));
  assert.ok(within(details).getByText(/hello/));
  assert.ok(within(details).getByText(/world/));
  await user.keyboard("{Escape}");
  await waitFor(() => assert.equal(screen.queryByRole("dialog"), null));
  await user.click(
    screen.getByRole("button", { name: /Dashboard time range: Last 24 hours/ }),
  );
  await user.click(await screen.findByRole("button", { name: "Last 7 days" }));
  await waitFor(() => assert.equal(ranges.length, 2));
  assert.equal(ranges[0][1] - ranges[0][0], 24 * 3600);
  assert.ok(Math.abs(ranges[1][1] - ranges[1][0] - 7 * 24 * 3600) <= 1);
  assert.equal(screen.queryByRole("dialog"), null);
  await user.click(
    (await screen.findAllByRole("button", { name: "View request 42" }))[0],
  );
  assert.ok(await screen.findByText("Request details"));
});
