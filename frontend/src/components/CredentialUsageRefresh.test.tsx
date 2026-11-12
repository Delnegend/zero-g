import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type * as TestingLibrary from "@testing-library/react/pure";
import type * as CredentialUsageModule from "./CredentialUsage";
import { TooltipProvider } from "./ui/tooltip";

let originalFetch: typeof fetch;
let testingLibrary: typeof TestingLibrary;
let CredentialUsage: typeof CredentialUsageModule.CredentialUsage;
let CredentialUsageRefreshButton: typeof CredentialUsageModule.CredentialUsageRefreshButton;

beforeEach(async () => {
  document.body.innerHTML = "";
  originalFetch = globalThis.fetch;
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
    value: true,
    configurable: true,
    writable: true,
  });
  // Testing Library binds its global screen to the document at module load.
  testingLibrary = await import("@testing-library/react/pure");
  ({ CredentialUsage, CredentialUsageRefreshButton } =
    await import("./CredentialUsage"));
});

afterEach(() => {
  testingLibrary.cleanup();
  globalThis.fetch = originalFetch;
  document.body.innerHTML = "";
});

function mount(connected = true) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return testingLibrary.render(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <CredentialUsage
          providerId="zen"
          credentialId="account"
          credentialName="Account"
          providerKind="zen"
          connected={connected}
        />
        <CredentialUsageRefreshButton
          providerId="zen"
          credentialId="account"
          connected={connected}
        />
      </TooltipProvider>
    </QueryClientProvider>,
  );
}

function balance(amount: number) {
  return Response.json({ kind: "balance", amount, currency: "USD" });
}

test("refresh fetches current usage on demand and keeps the old balance visible while loading", async () => {
  let calls = 0;
  // The project targets ES2023, before Promise.withResolvers.
  let finishRefresh!: (response: Response) => void;
  const refresh = new Promise<Response>((resolve) => {
    finishRefresh = resolve;
  });
  globalThis.fetch = (async (url) => {
    assert.equal(url, "/api/providers/zen/credentials/account/usage");
    calls++;
    if (calls === 1) return balance(10);
    return refresh;
  }) as typeof fetch;

  const view = mount();
  assert.ok(await view.findByText("$10.00"));
  assert.equal(calls, 1);
  testingLibrary.fireEvent.click(
    view.getByRole("button", { name: "Refresh usage" }),
  );
  const refreshing = await view.findByRole("button", { name: "Refreshing…" });
  assert.equal((refreshing as HTMLButtonElement).disabled, true);
  assert.ok(view.getByText("$10.00"));
  testingLibrary.fireEvent.click(refreshing);
  assert.equal(calls, 2);
  await testingLibrary.act(async () => finishRefresh(balance(7)));
  assert.ok(await view.findByText("$7.00"));
  assert.ok(view.getByRole("button", { name: "Refresh usage" }));
  assert.equal(calls, 2);
});

test("refresh updates quota windows without remounting the credential", async () => {
  let calls = 0;
  globalThis.fetch = (async () => {
    calls++;
    return Response.json({
      kind: "quota",
      windows: [
        {
          label: "Weekly",
          remaining_percent: calls === 1 ? 80 : 45,
          reset_at: null,
        },
      ],
    });
  }) as typeof fetch;

  const view = mount();
  assert.ok(await view.findByText("80% remaining"));
  testingLibrary.fireEvent.click(
    view.getByRole("button", { name: "Refresh usage" }),
  );
  assert.ok(await view.findByText("45% remaining"));
  assert.equal(calls, 2);
});

test("failed refresh retains the last usage and can be retried", async () => {
  let calls = 0;
  globalThis.fetch = (async () => {
    calls++;
    if (calls === 1) return balance(10);
    if (calls === 2)
      return new Response("Provider unavailable", { status: 502 });
    return balance(8);
  }) as typeof fetch;

  const view = mount();
  assert.ok(await view.findByText("$10.00"));
  testingLibrary.fireEvent.click(
    view.getByRole("button", { name: "Refresh usage" }),
  );
  assert.match(
    (await view.findByRole("alert")).textContent ?? "",
    /Refresh failed: Provider unavailable/,
  );
  assert.ok(view.getByText("$10.00"));
  testingLibrary.fireEvent.click(view.getByRole("button", { name: "Retry" }));
  assert.ok(await view.findByText("$8.00"));
  assert.equal(view.queryByRole("alert"), null);
  assert.equal(calls, 3);
});

test("an initial failure can be retried and disconnected credentials do not fetch", async () => {
  let calls = 0;
  globalThis.fetch = (async () => {
    calls++;
    return calls === 1
      ? new Response("Provider unavailable", { status: 502 })
      : balance(5);
  }) as typeof fetch;

  const view = mount(false);
  assert.equal(calls, 0);
  assert.equal(view.queryByRole("button", { name: "Refresh usage" }), null);
  view.unmount();
  const connectedView = mount();
  assert.match(
    (await connectedView.findByRole("alert")).textContent ?? "",
    /Usage unavailable: Provider unavailable/,
  );
  testingLibrary.fireEvent.click(
    connectedView.getByRole("button", { name: "Retry" }),
  );
  assert.ok(await connectedView.findByText("$5.00"));
  await testingLibrary.waitFor(() => assert.equal(calls, 2));
});
