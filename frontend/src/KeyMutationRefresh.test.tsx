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
  "localStorage",
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
Object.defineProperty(globalThis, "scrollTo", {
  value() {},
  configurable: true,
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
for (const target of [globalThis, window]) {
  Object.defineProperty(target, "ResizeObserver", {
    value: ObserverStub,
    configurable: true,
  });
  Object.defineProperty(target, "IntersectionObserver", {
    value: ObserverStub,
    configurable: true,
  });
  Object.defineProperty(target, "PointerEvent", {
    value: dom.window.MouseEvent,
    configurable: true,
  });
  Object.defineProperty(target, "requestAnimationFrame", {
    value: (callback: FrameRequestCallback) =>
      setTimeout(() => callback(Date.now()), 0),
    configurable: true,
  });
  Object.defineProperty(target, "cancelAnimationFrame", {
    value: clearTimeout,
    configurable: true,
  });
}
Object.defineProperty(dom.window.HTMLElement.prototype, "scrollIntoView", {
  value() {},
  configurable: true,
});

// Testing Library and Radix capture the document at import time, after JSDOM setup.
const { act, render, within, cleanup, waitFor, getQueriesForElement } =
  await import("@testing-library/react");
const screen = getQueriesForElement(dom.window.document.body);
const userEvent = (await import("@testing-library/user-event")).default;
const { KeysPage } = await import("./routes/keys");
const { TooltipProvider } = await import("./components/ui/tooltip");
const originalFetch = globalThis.fetch;

afterEach(() => {
  cleanup();
  globalThis.fetch = originalFetch;
});

function setup(operation: "create" | "revoke" | "expiration") {
  let mutations = 0;
  let failRefresh = false;
  let keys =
    operation === "create"
      ? []
      : [
          {
            id: "key-1",
            name: "CI key",
            providers: [] as string[],
            models: [] as string[],
            created_at: 1,
            expires_at: 4102444800 as number | null,
            revoked_at: null as number | null,
          },
        ];
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    const method = options?.method ?? "GET";
    if (path === "/api/providers") return Response.json({ providers: [] });
    if (path === "/api/model-catalog")
      return Response.json({ providers: [], datasheets_error: null });
    if (path === "/api/keys" && method === "GET") {
      if (failRefresh) {
        failRefresh = false;
        return new Response("List unavailable", { status: 503 });
      }
      return Response.json({ keys });
    }
    if (path === "/api/keys" && method === "POST") {
      mutations++;
      const key = {
        id: "key-1",
        name: "CI key",
        providers: [],
        models: [],
        created_at: 1,
        expires_at: null,
        revoked_at: null,
      };
      keys = [key];
      failRefresh = true;
      return Response.json({ ...key, key: "secret-once" });
    }
    if (path === "/api/keys/key-1/revoke" && method === "POST") {
      mutations++;
      keys = [{ ...keys[0], revoked_at: 1 }];
      failRefresh = true;
      return new Response(null, { status: 204 });
    }
    if (path === "/api/keys/key-1" && method === "PATCH") {
      mutations++;
      keys = [
        {
          ...keys[0],
          expires_at: JSON.parse(String(options?.body)).expires_at,
        },
      ];
      failRefresh = true;
      return new Response(null, { status: 204 });
    }
    throw new Error(`Unexpected request: ${method} ${path}`);
  }) as typeof fetch;
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <KeysPage />
      </TooltipProvider>
    </QueryClientProvider>,
  );
  return { client, mutations: () => mutations };
}

test("created token survives a failed list refresh and retry does not create another key", async () => {
  const state = setup("create");
  const user = userEvent.setup({ document: dom.window.document });
  await screen.findByText("No keys created yet.");
  await user.click(screen.getByRole("button", { name: "Create key" }));
  const dialog = await screen.findByRole("dialog");
  const submit = within(dialog).getByRole("button", { name: "Create key" });
  await waitFor(() =>
    assert.equal((submit as HTMLButtonElement).disabled, false),
  );
  const name = within(dialog).getByLabelText("Name") as HTMLInputElement;
  await user.type(name, "CI key");
  assert.equal(name.value, "CI key");
  await user.click(submit);
  await screen.findByText("secret-once");
  await screen.findByText(/key list could not be refreshed/);
  const tokenDialog = screen.getByRole("dialog");
  assert.ok(within(tokenDialog).getByText("Key created successfully."));
  assert.equal(
    within(tokenDialog).queryByRole("button", { name: "Create key" }),
    null,
  );
  await user.click(
    within(tokenDialog).getByRole("button", { name: "Retry key list" }),
  );
  await waitFor(() =>
    assert.equal(
      within(tokenDialog).queryByText(/key list could not be refreshed/),
      null,
    ),
  );
  assert.ok(within(tokenDialog).getByText("secret-once"));
  await user.click(
    within(tokenDialog).getByRole("button", { name: "Dismiss" }),
  );
  await screen.findByText("CI key");
  assert.equal(state.mutations(), 1);
});

test("a later independent key-list refetch clears the stale-list warning", async () => {
  const state = setup("revoke");
  const user = userEvent.setup({ document: dom.window.document });
  await screen.findByText("CI key");
  await user.click(screen.getByRole("button", { name: "Revoke CI key" }));
  const dialog = await screen.findByRole("dialog");
  await user.click(within(dialog).getByRole("button", { name: "Revoke" }));
  await screen.findByText(/key list could not be refreshed/);
  await act(async () => {
    await state.client.refetchQueries({ queryKey: ["virtual-keys"] });
  });
  await screen.findByText("Revoked");
  await waitFor(() =>
    assert.equal(screen.queryByText(/key list could not be refreshed/), null),
  );
  assert.ok(screen.getByText("Key revoked successfully."));
  assert.equal(state.mutations(), 1);
});

for (const operation of ["revoke", "expiration"] as const) {
  test(`${operation} succeeds even when the key list refresh fails`, async () => {
    const state = setup(operation);
    const user = userEvent.setup({ document: dom.window.document });
    await screen.findByText("CI key");
    await user.click(
      screen.getByRole("button", {
        name:
          operation === "revoke"
            ? "Revoke CI key"
            : "Edit expiration for CI key",
      }),
    );
    const dialog = await screen.findByRole("dialog");
    if (operation === "expiration") {
      await user.click(
        within(dialog).getByRole("button", { name: /Choose expiration date/ }),
      );
      await user.click(
        screen.getByRole("button", { name: "Clear expiration" }),
      );
    }
    await user.click(
      within(dialog).getByRole("button", {
        name: operation === "revoke" ? "Revoke" : "Save expiration",
      }),
    );
    await screen.findByText(/key list could not be refreshed/);
    assert.equal(screen.queryByRole("dialog"), null);
    assert.ok(
      screen.getByText(
        operation === "revoke"
          ? "Key revoked successfully."
          : "Key expiration updated successfully.",
      ),
    );
    assert.equal(screen.queryByRole("button", { name: "Revoke CI key" }), null);
    await user.click(screen.getByRole("button", { name: "Retry key list" }));
    await waitFor(() =>
      assert.equal(screen.queryByText(/key list could not be refreshed/), null),
    );
    if (operation === "revoke") {
      await screen.findByText("Revoked");
      assert.equal(
        (
          screen.getByRole("button", {
            name: "Revoke CI key",
          }) as HTMLButtonElement
        ).disabled,
        true,
      );
    } else {
      await screen.findByText("Never expires");
    }
    assert.equal(state.mutations(), 1);
  });
}
