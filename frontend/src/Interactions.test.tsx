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
class ObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
Object.defineProperty(globalThis, "ResizeObserver", {
  value: ObserverStub,
  configurable: true,
});
Object.defineProperty(window, "ResizeObserver", {
  value: ObserverStub,
  configurable: true,
});
Object.defineProperty(globalThis, "IntersectionObserver", {
  value: ObserverStub,
  configurable: true,
});
Object.defineProperty(window, "IntersectionObserver", {
  value: ObserverStub,
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
Object.defineProperty(window, "requestAnimationFrame", {
  value: globalThis.requestAnimationFrame,
  configurable: true,
});
Object.defineProperty(window, "cancelAnimationFrame", {
  value: globalThis.cancelAnimationFrame,
  configurable: true,
});

// These modules capture the browser document at import time, so load them after JSDOM setup.
const { act, render, screen, within, cleanup, waitFor } =
  await import("@testing-library/react");
const userEvent = (await import("@testing-library/user-event")).default;
const { Management } = await import("./Management");
const { ObservabilityPage } = await import("./App");
const { Credentials } = await import("./components/providers/Credentials");
const { CredentialUsage } = await import("./components/CredentialUsage");
const { ProviderCard } = await import("./components/providers/ProviderCard");
const { ProvidersPage } = await import("./ProvidersPage");
const { ProviderForm } = await import("./components/providers/ProviderForm");
const { ModelCatalogPage, ModelSettingsPage } = await import("./Models");
const { TooltipProvider } = await import("./components/ui/tooltip");
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

const settings = { log_retention_days: 30 };
test("provider save stays open on failure and closes after a successful retry", async () => {
  let attempts = 0;
  let closed = false;
  globalThis.fetch = (async (_url, options) => {
    if (options?.method === "POST") {
      attempts++;
      return attempts === 1
        ? new Response("Save failed", { status: 500 })
        : Response.json({
            id: "codex",
            name: "Codex",
            kind: "codex",
            base_url: "",
          });
    }
    throw new Error("Unexpected request");
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <ProviderForm
      kinds={[
        {
          value: "codex",
          label: "Codex",
          api_key: false,
          oauth: true,
          fixed_id: "codex",
          fixed_name: "Codex",
        },
      ]}
      onDone={() => {
        closed = true;
      }}
      onCancel={() => {}}
    />,
  );
  await user.click(screen.getByRole("button", { name: "Add provider" }));
  assert.ok(await screen.findByRole("alert"));
  assert.equal(closed, false);
  await user.click(screen.getByRole("button", { name: "Add provider" }));
  await waitFor(() => assert.equal(closed, true));
  assert.equal(attempts, 2);
});

test("model settings retain edits made while saving and clear success on edit", async () => {
  const initial = {
    pricing_url: "",
    parameters_url: "",
    pricing_interval_hours: 1,
    discovery_interval_minutes: 5,
  };
  let release!: (response: Response) => void;
  globalThis.fetch = (async (_url, options) => {
    if (options?.method === "PUT") {
      return new Promise<Response>((resolve) => {
        release = resolve;
      });
    }
    return Response.json(initial);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<ModelSettingsPage />);
  const pricing = await screen.findByLabelText("Pricing datasheet URL");
  await user.type(pricing, "https://example.test/first");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  assert.ok(
    (screen.getByRole("button", { name: "Saving…" }) as HTMLButtonElement)
      .disabled,
  );
  await user.type(pricing, "-edited");
  release(
    Response.json({ ...initial, pricing_url: "https://example.test/first" }),
  );
  await screen.findByText("Settings saved.");
  assert.equal(
    (pricing as HTMLInputElement).value,
    "https://example.test/first-edited",
  );
  await user.type(pricing, "x");
  assert.equal(screen.queryByText("Settings saved."), null);
});

test("settings save log retention without provider OAuth settings", async () => {
  const bodies: unknown[] = [];
  globalThis.fetch = (async (_url, options) => {
    if (options?.method === "PUT")
      bodies.push(JSON.parse(String(options.body)));
    return Response.json(settings);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<Management page="settings" />);
  await screen.findByDisplayValue("30");
  assert.equal(screen.queryByLabelText(/Google OAuth/), null);
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await screen.findByText("Settings saved.");
  assert.deepEqual(bodies, [{ log_retention_days: 30 }]);
});

test("edits made during a delayed settings save survive the response", async () => {
  const bodies: unknown[] = [];
  let release!: (response: Response) => void;
  const delayed = new Promise<Response>((resolve) => {
    release = resolve;
  });
  globalThis.fetch = (async (_url, options) => {
    if (options?.method !== "PUT") return Response.json(settings);
    bodies.push(JSON.parse(String(options.body)));
    if (bodies.length === 1) return delayed;
    return Response.json({ log_retention_days: 40 });
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<Management page="settings" />);
  const retention = await screen.findByLabelText("Log retention (days)");
  await user.clear(retention);
  await user.type(retention, "35");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => assert.equal(bodies.length, 1));
  await user.clear(retention);
  await user.type(retention, "40");
  release(Response.json({ log_retention_days: 35 }));
  await screen.findByText("Settings saved.");
  assert.equal((retention as HTMLInputElement).value, "40");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => assert.equal(bodies.length, 2));
  assert.deepEqual(bodies, [
    { log_retention_days: 35 },
    { log_retention_days: 40 },
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
  await user.click(screen.getByRole("button", { name: "Edit Production" }));
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
  await user.click(screen.getByRole("button", { name: "Edit Renamed" }));
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
  await user.click(screen.getByRole("button", { name: "Delete Renamed" }));
  assert.ok(await screen.findByRole("dialog", { name: "Delete credential?" }));
  await user.click(screen.getByRole("button", { name: "Cancel" }));
});

test("provider deletion stays unavailable until all credentials are removed", async () => {
  let present = true;
  let release!: (response: Response) => void;
  let loads = 0;
  globalThis.fetch = (async (_url, options) => {
    if (options?.method === "DELETE") {
      present = false;
      return new Response(null, { status: 204 });
    }
    loads++;
    const value = {
      credentials: present
        ? [
            {
              id: "one",
              name: "Disabled",
              weight: 1,
              enabled: false,
              connected: false,
            },
          ]
        : [],
    };
    if (loads === 1)
      return new Promise<Response>((resolve) => {
        release = resolve;
      });
    return Response.json(value);
  }) as typeof fetch;
  let deleted = false;
  const user = userEvent.setup();
  mount(
    <ProviderCard
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
      onEdit={() => {}}
      onDelete={() => {
        deleted = true;
      }}
      deleting={false}
    />,
  );
  const deleteProvider = screen.getByRole("button", {
    name: "Delete Alpha",
  }) as HTMLButtonElement;
  assert.equal(deleteProvider.disabled, true);
  await waitFor(() => assert.equal(loads, 1));
  release(
    Response.json({
      credentials: [
        {
          id: "one",
          name: "Disabled",
          weight: 1,
          enabled: false,
          connected: false,
        },
      ],
    }),
  );
  await screen.findByText(
    "Remove all credentials before deleting this provider.",
  );
  assert.equal(deleteProvider.disabled, true);
  await user.click(screen.getByRole("button", { name: "Delete Disabled" }));
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Delete credential",
    }),
  );
  await waitFor(() => assert.equal(deleteProvider.disabled, false));
  await user.click(deleteProvider);
  assert.equal(deleted, true);
});

test("provider deletion remains disabled when credentials cannot be loaded", async () => {
  globalThis.fetch = (async () =>
    new Response("Credentials unavailable", { status: 503 })) as typeof fetch;
  mount(
    <ProviderCard
      provider={{
        id: "alpha",
        name: "Alpha",
        kind: "openai",
        base_url: "https://example.test",
      }}
      onEdit={() => {}}
      onDelete={() => {}}
      deleting={false}
    />,
  );
  assert.ok(await screen.findByText("Credentials unavailable"));
  assert.equal(
    (screen.getByRole("button", { name: "Delete Alpha" }) as HTMLButtonElement)
      .disabled,
    true,
  );
});

test("provider deletion reports a conflict if credentials were added elsewhere", async () => {
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    if (path === "/api/providers" && options?.method === "GET")
      return Response.json({
        providers: [
          {
            id: "alpha",
            name: "Alpha",
            kind: "openai",
            base_url: "https://example.test",
          },
        ],
      });
    if (path === "/api/provider-kinds")
      return Response.json({
        kinds: [
          {
            value: "openai",
            label: "OpenAI",
            api_key: true,
            oauth: false,
            fixed_id: null,
            fixed_name: null,
          },
        ],
      });
    if (path.endsWith("/credentials"))
      return Response.json({ credentials: [] });
    if (path === "/api/providers/alpha" && options?.method === "DELETE")
      return new Response(
        "Remove all credentials before deleting this provider",
        { status: 409 },
      );
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<ProvidersPage />);
  const button = await screen.findByRole("button", { name: "Delete Alpha" });
  await waitFor(() =>
    assert.equal((button as HTMLButtonElement).disabled, false),
  );
  await user.click(button);
  await user.click(
    within(await screen.findByRole("dialog")).getByRole("button", {
      name: "Delete provider",
    }),
  );
  assert.ok(
    await screen.findByText(
      "Remove all credentials before deleting this provider",
    ),
  );
  assert.ok(screen.getByRole("dialog"));
});

test("credential names show available account emails and plans without placeholders", async () => {
  globalThis.fetch = (async (url) =>
    String(url).endsWith("/usage")
      ? Response.json({ kind: "grouped_quota", groups: [] })
      : Response.json({
          credentials: [
            {
              id: "one",
              name: "Personal",
              email: "person@example.test",
              plan_type: "pro",
              weight: 1,
              enabled: true,
              connected: true,
            },
            {
              id: "two",
              name: "Work",
              weight: 1,
              enabled: true,
              connected: false,
            },
            {
              id: "three",
              name: "Anonymous",
              plan_type: "plus",
              weight: 1,
              enabled: true,
              connected: false,
            },
          ],
        })) as typeof fetch;
  mount(
    <Credentials
      provider={{
        id: "codex",
        name: "OpenAI Codex",
        kind: "codex",
        base_url: "",
      }}
      kind={{
        value: "codex",
        label: "Codex",
        fixed_id: "codex",
        fixed_name: "OpenAI Codex",
        oauth: true,
        api_key: false,
      }}
      onLogin={() => {}}
      loginBusy={false}
    />,
  );
  assert.ok(await screen.findByText("person@example.test", { exact: false }));
  assert.ok(screen.getByText("pro", { exact: true }));
  assert.ok(screen.getByText("plus", { exact: true }));
  assert.ok(screen.getByText("Work", { exact: true }));
  assert.equal(screen.queryByText(/Work •/), null);
});

test("OpenRouter credentials configure and remove a management key and show balance", async () => {
  const requests: { url: string; method: string; body: unknown }[] = [];
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    const method = options?.method ?? "GET";
    if (method === "PUT" || method === "DELETE") {
      requests.push({
        url: path,
        method,
        body: options?.body ? JSON.parse(String(options.body)) : undefined,
      });
      if (method === "PUT" && requests.length === 1) {
        return new Response(null, { status: 500 });
      }
      return new Response(null, { status: 204 });
    }
    if (path.endsWith("/usage")) {
      return Response.json({ kind: "balance", amount: 12.5, currency: "USD" });
    }
    return Response.json({
      credentials: [
        {
          id: "account",
          name: "Account",
          weight: 1,
          enabled: true,
          connected: true,
        },
      ],
    });
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <Credentials
      provider={{
        id: "openrouter",
        name: "OpenRouter",
        kind: "openrouter",
        base_url: "https://openrouter.ai/api/v1",
      }}
      kind={{
        value: "openrouter",
        label: "OpenRouter",
        api_key: true,
        oauth: false,
        fixed_id: "openrouter",
        fixed_name: "OpenRouter",
      }}
      onLogin={() => {}}
      loginBusy={false}
    />,
  );
  assert.ok(await screen.findByText("$12.50"));
  assert.equal(
    screen.getByRole("button", { name: "Configure management key" })
      .parentElement,
    screen.getByRole("button", { name: "Delete Account" }).parentElement,
  );
  await user.click(
    screen.getByRole("button", { name: "Configure management key" }),
  );
  await user.type(
    screen.getByLabelText("Management key"),
    "test-management-key",
  );
  await user.click(screen.getByRole("button", { name: "Save management key" }));
  assert.ok(
    await screen.findByText("Could not save the management key. Try again."),
  );
  assert.equal(
    (
      screen.getByRole("button", {
        name: "Save management key",
      }) as HTMLButtonElement
    ).disabled,
    false,
  );
  await user.click(screen.getByRole("button", { name: "Save management key" }));
  await waitFor(() => assert.equal(requests.length, 2));
  assert.deepEqual(requests[1], {
    url: "/api/providers/openrouter/credentials/account/openrouter/management-key",
    method: "PUT",
    body: { management_key: "test-management-key" },
  });
  await waitFor(() => assert.equal(screen.queryByRole("dialog"), null));
  await user.click(
    screen.getByRole("button", { name: "Configure management key" }),
  );
  await user.click(
    screen.getByRole("button", { name: "Remove management key" }),
  );
  await waitFor(() => assert.equal(requests.length, 3));
  assert.deepEqual(requests[2], {
    url: "/api/providers/openrouter/credentials/account/openrouter/management-key",
    method: "DELETE",
    body: undefined,
  });
});

test("Zen console configuration sits with credential actions and can be saved", async () => {
  let saved = false;
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    if (path.endsWith("/console") && options?.method === "PUT") {
      saved = true;
      return new Response(null, { status: 204 });
    }
    if (path.endsWith("/usage"))
      return Response.json({ kind: "balance", amount: 10, currency: "USD" });
    return Response.json({
      credentials: [
        {
          id: "account",
          name: "Account",
          weight: 1,
          enabled: true,
          connected: true,
        },
      ],
    });
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <Credentials
      provider={{ id: "zen", name: "OpenCode Zen", kind: "zen", base_url: "" }}
      kind={{
        value: "zen",
        label: "Zen",
        api_key: true,
        oauth: false,
        fixed_id: "zen",
        fixed_name: "OpenCode Zen",
      }}
      onLogin={() => {}}
      loginBusy={false}
    />,
  );
  const configure = await screen.findByRole("button", {
    name: "Configure console",
  });
  assert.equal(
    configure.parentElement,
    screen.getByRole("button", { name: "Delete Account" }).parentElement,
  );
  await user.click(configure);
  await user.type(screen.getByLabelText("Workspace ID"), "workspace");
  await user.type(screen.getByLabelText("Console session"), "test-session");
  await user.click(screen.getByRole("button", { name: "Save session" }));
  await waitFor(() => assert.equal(saved, true));
  await waitFor(() => assert.equal(screen.queryByRole("dialog"), null));
});

test("quota resets show a countdown and reveal the exact time", async () => {
  const reset = Math.floor(Date.now() / 1000) + 2 * 3600 + 45;
  globalThis.fetch = (async () =>
    Response.json({
      kind: "quota",
      windows: [
        { label: "5 hours", remaining_percent: null, reset_at: reset },
        { label: "Weekly", remaining_percent: null, reset_at: reset - 86400 },
      ],
    })) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <CredentialUsage
        providerId="codex"
        credentialId="account"
        credentialName="Account"
        kind="codex"
        connected
      />
    </TooltipProvider>,
  );
  const trigger = await screen.findByRole("button", {
    name: "Resets in 2h 1m",
  });
  assert.ok(screen.getByRole("button", { name: "Reset due" }));
  await user.hover(trigger);
  assert.ok(await screen.findByText(new Date(reset * 1000).toLocaleString()));
});

test("Antigravity quotas show each pool with its own windows", async () => {
  globalThis.fetch = (async () =>
    Response.json({
      kind: "grouped_quota",
      groups: [
        {
          label: "Gemini Models",
          windows: [
            { label: "5 hours", remaining_percent: 80, reset_at: null },
            { label: "Weekly", remaining_percent: 40, reset_at: null },
          ],
        },
        {
          label: "Claude and GPT models",
          windows: [
            { label: "5 hours", remaining_percent: 30, reset_at: null },
            { label: "Weekly", remaining_percent: null, reset_at: null },
            { label: "Daily", remaining_percent: 0, reset_at: null },
            { label: "Monthly", remaining_percent: 100, reset_at: null },
          ],
        },
      ],
    })) as typeof fetch;
  mount(
    <CredentialUsage
      providerId="agy"
      credentialId="account"
      credentialName="Account"
      kind="agy"
      connected
    />,
  );
  const gemini = await screen.findByRole("group", { name: "Gemini Models" });
  const claude = screen.getByRole("group", { name: "Claude and GPT models" });
  assert.ok(within(gemini).getByText("80% remaining"));
  assert.ok(within(gemini).getByText("40% remaining"));
  assert.ok(within(claude).getByText("30% remaining"));
  assert.ok(within(claude).getByText("Unknown remaining"));
  assert.equal(
    within(gemini)
      .getByRole("progressbar", { name: "5 hours remaining" })
      .getAttribute("aria-valuenow"),
    "80",
  );
  assert.equal(
    within(gemini)
      .getByRole("progressbar", { name: "Weekly remaining" })
      .getAttribute("aria-valuenow"),
    "40",
  );
  assert.equal(
    within(claude).queryByRole("progressbar", { name: "Weekly remaining" }),
    null,
  );
  assert.equal(
    within(claude)
      .getByRole("progressbar", { name: "Daily remaining" })
      .getAttribute("aria-valuenow"),
    "0",
  );
  assert.equal(
    within(claude)
      .getByRole("progressbar", { name: "Monthly remaining" })
      .getAttribute("aria-valuenow"),
    "100",
  );
});

test("connected Claude credentials show subscription quota", async () => {
  globalThis.fetch = (async (url) =>
    String(url).endsWith("/usage")
      ? Response.json({
          kind: "grouped_quota",
          groups: [
            {
              label: "Claude subscription",
              windows: [
                { label: "5 hours", remaining_percent: 80, reset_at: null },
                { label: "Weekly", remaining_percent: 60, reset_at: null },
              ],
            },
          ],
        })
      : Response.json({
          credentials: [
            {
              id: "account",
              name: "Account",
              weight: 1,
              enabled: true,
              connected: true,
            },
          ],
        })) as typeof fetch;
  mount(
    <Credentials
      provider={{
        id: "claude",
        name: "Claude Code",
        kind: "claude",
        base_url: "",
      }}
      kind={{
        value: "claude",
        label: "Claude Code",
        api_key: false,
        oauth: true,
        fixed_id: "claude",
        fixed_name: "Claude Code",
      }}
      onLogin={() => {}}
      loginBusy={false}
    />,
  );
  const group = await screen.findByRole("group", {
    name: "Claude subscription",
  });
  assert.ok(within(group).getByText("80% remaining"));
  assert.ok(within(group).getByText("60% remaining"));
});

test("Codex device code copies with temporary confirmation", async () => {
  globalThis.fetch = (async (url) => {
    const path = String(url);
    if (path.endsWith("/credentials")) {
      return Response.json({
        credentials: [
          {
            id: "one",
            name: "Account",
            weight: 1,
            enabled: true,
            connected: false,
          },
        ],
      });
    }
    if (path.endsWith("/codex/start")) {
      return Response.json({
        id: "flow",
        url: "https://auth.openai.com/device",
        user_code: "ABCD-1234",
        interval: 60,
      });
    }
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <ProviderCard
        provider={{ id: "codex", name: "Codex", kind: "codex", base_url: "" }}
        kind={{
          value: "codex",
          label: "Codex",
          api_key: false,
          oauth: true,
          fixed_id: "codex",
          fixed_name: "Codex",
        }}
        onEdit={() => {}}
        onDelete={() => {}}
        deleting={false}
      />
    </TooltipProvider>,
  );
  await user.click(
    await screen.findByRole("button", { name: "Sign in Codex" }),
  );
  const button = await screen.findByRole("button", {
    name: "Copy device code ABCD-1234",
  });
  await user.hover(button);
  assert.ok(await screen.findByText("Copy", { exact: true }));
  await user.click(button);
  assert.equal(await navigator.clipboard.readText(), "ABCD-1234");
  assert.ok(screen.getByText("Copied", { exact: true }));
  assert.ok(await screen.findByText("Copied!", { exact: true }));
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 1100));
  });
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 200));
  });
  assert.equal(screen.queryByText("Copied", { exact: true }), null);
});

for (const [kind, label, host] of [
  ["claude", "Claude Code", "claude.ai"],
  ["cursor", "Cursor", "cursor.com"],
  ["grok", "Grok", "auth.x.ai"],
  ["grok-build", "Grok Build", "auth.x.ai"],
] as const) {
  test(`${label} sign-in uses its credential-scoped flow`, async () => {
    const requests: Array<{ path: string; body: unknown }> = [];
    const prefix = `/api/providers/alpha/credentials/credential-1/${kind}`;
    globalThis.fetch = (async (url, options) => {
      const path = String(url);
      if (path === "/api/providers/alpha/credentials")
        return Response.json({
          credentials: [
            {
              id: "credential-1",
              name: "Account",
              weight: 1,
              enabled: true,
              connected: false,
            },
          ],
        });
      requests.push({
        path,
        body: options?.body ? JSON.parse(String(options.body)) : null,
      });
      if (path === `${prefix}/start`)
        return Response.json({
          id: "flow-1",
          url: `https://${host}/authorize`,
          interval: 1,
          user_code: "ABCD",
        });
      if (path === `${prefix}/poll` || path === `${prefix}/finish`)
        return Response.json({ status: "complete" });
      throw new Error(`Unexpected request: ${path}`);
    }) as typeof fetch;
    const user = userEvent.setup();
    mount(
      <TooltipProvider>
        <ProviderCard
          provider={{ id: "alpha", name: "Alpha", kind, base_url: "" }}
          kind={{
            value: kind,
            label,
            api_key: false,
            oauth: true,
            fixed_id: null,
            fixed_name: null,
          }}
          onEdit={() => {}}
          onDelete={() => {}}
          deleting={false}
        />
      </TooltipProvider>,
    );
    await user.click(
      await screen.findByRole("button", { name: `Sign in ${label}` }),
    );
    const link = await screen.findByRole("link", { name: /authorization/ });
    assert.equal(new URL(link.getAttribute("href")!).hostname, host);
    assert.equal(screen.queryByText(/quota/i), null);
    if (kind === "grok" || kind === "grok-build") {
      const code = screen.getByRole("button", {
        name: "Copy device code ABCD",
      });
      await user.hover(code);
      assert.ok(await screen.findByText("Copy", { exact: true }));
      await user.click(code);
      assert.equal(await navigator.clipboard.readText(), "ABCD");
      assert.ok(await screen.findByText("Copied!", { exact: true }));
    }
    if (kind === "claude") {
      await user.type(
        screen.getByLabelText("Authorization code or callback URL"),
        "secret-code",
      );
      await user.click(
        screen.getByRole("button", { name: "Finish Claude sign-in" }),
      );
      await screen.findByText("Sign-in complete.");
      assert.deepEqual(requests[1], {
        path: `${prefix}/finish`,
        body: { id: "flow-1", code: "secret-code" },
      });
    } else {
      if (kind !== "cursor") assert.ok(screen.getByText("ABCD"));
      await screen.findByText("Sign-in complete.", {}, { timeout: 3000 });
      assert.deepEqual(requests[1], {
        path: `${prefix}/poll`,
        body: { id: "flow-1" },
      });
    }
  });
}

test("Google callback failure keeps the flow available for retry", async () => {
  let attempts = 0;
  globalThis.fetch = (async (url) => {
    const path = String(url);
    if (path.endsWith("/credentials"))
      return Response.json({
        credentials: [
          {
            id: "one",
            name: "Account",
            weight: 1,
            enabled: true,
            connected: false,
          },
        ],
      });
    if (path.endsWith("/agy/start"))
      return Response.json({
        id: "flow",
        url: "https://accounts.google.com/authorize",
        local_callback: false,
      });
    if (path.endsWith("/agy/finish")) {
      attempts++;
      return attempts === 1
        ? new Response("Invalid callback", { status: 400 })
        : Response.json({ status: "complete" });
    }
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <ProviderCard
      provider={{ id: "agy", name: "Google", kind: "agy", base_url: "" }}
      kind={{
        value: "agy",
        label: "Google",
        api_key: false,
        oauth: true,
        fixed_id: "agy",
        fixed_name: "Google",
      }}
      onEdit={() => {}}
      onDelete={() => {}}
      deleting={false}
    />,
  );
  await user.click(
    await screen.findByRole("button", { name: "Sign in Google" }),
  );
  await user.type(
    await screen.findByLabelText("Redirected callback URL"),
    "http://localhost/callback?code=example",
  );
  await user.click(
    screen.getByRole("button", { name: "Finish Google sign-in" }),
  );
  assert.ok(await screen.findByRole("alert"));
  assert.ok(screen.getByLabelText("Redirected callback URL"));
  await user.click(
    screen.getByRole("button", { name: "Finish Google sign-in" }),
  );
  await screen.findByText("Sign-in complete.");
  assert.equal(attempts, 2);
});

test("cancelling Google sign-in aborts polling and ignores a late completion", async () => {
  let pollSignal: AbortSignal | null = null;
  let release!: (response: Response) => void;
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    if (path.endsWith("/credentials"))
      return Response.json({
        credentials: [
          {
            id: "one",
            name: "Account",
            weight: 1,
            enabled: true,
            connected: false,
          },
        ],
      });
    if (path.endsWith("/agy/start"))
      return Response.json({
        id: "flow",
        url: "https://accounts.google.com/authorize",
        local_callback: true,
      });
    if (path.endsWith("/agy/poll")) {
      pollSignal = options?.signal as AbortSignal;
      return new Promise<Response>((resolve) => {
        release = resolve;
      });
    }
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <ProviderCard
      provider={{ id: "agy", name: "Google", kind: "agy", base_url: "" }}
      kind={{
        value: "agy",
        label: "Google",
        api_key: false,
        oauth: true,
        fixed_id: "agy",
        fixed_name: "Google",
      }}
      onEdit={() => {}}
      onDelete={() => {}}
      deleting={false}
    />,
  );
  await user.click(
    await screen.findByRole("button", { name: "Sign in Google" }),
  );
  await waitFor(() => assert.ok(pollSignal), { timeout: 3000 });
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  assert.ok((pollSignal as unknown as AbortSignal).aborted);
  await act(async () => {
    release(Response.json({ status: "complete" }));
  });
  assert.equal(screen.queryByText("Sign-in complete."), null);
  assert.ok(screen.getByRole("button", { name: "Sign in Google" }));
});

test("Google sign-in completes through polling", async () => {
  globalThis.fetch = (async (url) => {
    const path = String(url);
    if (path.endsWith("/credentials"))
      return Response.json({
        credentials: [
          {
            id: "one",
            name: "Account",
            weight: 1,
            enabled: true,
            connected: false,
          },
        ],
      });
    if (path.endsWith("/agy/start"))
      return Response.json({
        id: "flow",
        url: "https://accounts.google.com/authorize",
        local_callback: true,
      });
    if (path.endsWith("/agy/poll"))
      return Response.json({ status: "complete" });
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <ProviderCard
      provider={{ id: "agy", name: "Google", kind: "agy", base_url: "" }}
      kind={{
        value: "agy",
        label: "Google",
        api_key: false,
        oauth: true,
        fixed_id: "agy",
        fixed_name: "Google",
      }}
      onEdit={() => {}}
      onDelete={() => {}}
      deleting={false}
    />,
  );
  await user.click(
    await screen.findByRole("button", { name: "Sign in Google" }),
  );
  await screen.findByText("Sign-in complete.", {}, { timeout: 3000 });
  assert.equal(screen.queryByLabelText("Redirected callback URL"), null);
  assert.ok(screen.getByRole("button", { name: "Sign in Google" }));
});

test("rejects an untrusted provider authorization URL", async () => {
  globalThis.fetch = (async (url) =>
    String(url).endsWith("/credentials")
      ? Response.json({
          credentials: [
            {
              id: "credential-1",
              name: "Account",
              weight: 1,
              enabled: true,
              connected: false,
            },
          ],
        })
      : Response.json({
          id: "flow-1",
          url: "https://evil.example/authorize",
          interval: 1,
        })) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <ProviderCard
      provider={{ id: "alpha", name: "Alpha", kind: "cursor", base_url: "" }}
      kind={{
        value: "cursor",
        label: "Cursor",
        api_key: false,
        oauth: true,
        fixed_id: null,
        fixed_name: null,
      }}
      onEdit={() => {}}
      onDelete={() => {}}
      deleting={false}
    />,
  );
  await user.click(
    await screen.findByRole("button", { name: "Sign in Cursor" }),
  );
  await screen.findByText("Invalid Cursor authorization URL.");
  assert.equal(screen.queryByRole("link", { name: /authorization/ }), null);
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

test("model cards label supported input icons and retain unknown modalities", async () => {
  globalThis.fetch = (async () =>
    Response.json({
      datasheets_error: null,
      providers: [
        {
          id: "example",
          name: "Example",
          kind: "agy",
          base_url: "",
          error: null,
          models: [
            {
              model: { id: "model" },
              display_name: "Model",
              description: null,
              reasoning_efforts: [],
              reasoning: null,
              tool_call: null,
              input_modalities: [
                "text",
                "image",
                "audio",
                "video",
                "pdf",
                "geometry",
                "toString",
              ],
              pricing: null,
              raw_pricing: null,
              parameters: null,
              provider_metadata: null,
            },
          ],
        },
      ],
    })) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <ModelCatalogPage />
    </TooltipProvider>,
  );
  for (const modality of ["text", "image", "audio", "video", "pdf"]) {
    assert.ok(await screen.findByRole("button", { name: `${modality} input` }));
  }
  assert.ok(screen.getByText("geometry"));
  assert.ok(screen.getByText("toString"));
  await user.click(
    screen.getByRole("button", { name: "About Antigravity input support" }),
  );
  assert.ok(
    await screen.findByText(/forwards inline audio, video, and PDF data/i),
  );
});
