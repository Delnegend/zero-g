import { test } from "node:test";
import assert from "node:assert/strict";
import {
  fireEvent,
  within,
  waitFor,
  screen,
  userEvent,
  KeysPage,
  TooltipProvider,
  mount,
} from "./interactionTestSetup";

test("create restricted keys, edit expiration, and retain revoked metadata", async () => {
  let creates = 0;
  let keys: Array<{
    id: string;
    name: string;
    providers: string[];
    models: string[];
    created_at: number;
    expires_at: number | null;
    revoked_at: number | null;
  }> = [];
  const card = (id: string) => ({
    model: { id },
    display_name: id,
    description: null,
    reasoning_efforts: [],
    reasoning: null,
    tool_call: null,
    input_modalities: ["text"],
    pricing: null,
    raw_pricing: null,
    parameters: null,
    provider_metadata: null,
  });
  globalThis.fetch = (async (url, options) => {
    const path = String(url);
    const method = options?.method ?? "GET";
    if (path === "/api/providers")
      return Response.json({
        providers: [
          {
            id: "alpha",
            name: "Alpha",
            kind: "openai",
            base_url: "https://example.test",
            keyless: false,
          },
          {
            id: "beta",
            name: "Beta",
            kind: "openai",
            base_url: "https://example.test",
            keyless: false,
          },
        ],
      });
    if (path === "/api/model-catalog")
      return Response.json({
        datasheets_error: null,
        providers: [
          {
            id: "alpha",
            name: "Alpha",
            kind: "openai",
            base_url: "",
            error: null,
            models: [card("model-a"), card("model-b")],
          },
          {
            id: "beta",
            name: "Beta",
            kind: "openai",
            base_url: "",
            error: null,
            models: [card("model-c")],
          },
        ],
      });
    if (path === "/api/keys" && method === "POST") {
      const body = JSON.parse(String(options?.body));
      creates++;
      const key = { id: "key-1", ...body, created_at: 1, revoked_at: null };
      keys = [key];
      return Response.json({ ...key, key: "secret-once" });
    }
    if (path === "/api/keys/key-1" && method === "PATCH") {
      const body = JSON.parse(String(options?.body));
      keys = [
        {
          ...keys[0],
          ...(body.name !== undefined ? { name: body.name } : {}),
          ...(body.providers !== undefined
            ? { providers: body.providers }
            : {}),
          ...(body.models !== undefined ? { models: body.models } : {}),
          ...(body.expires_at !== undefined
            ? { expires_at: body.expires_at }
            : {}),
        },
      ];
      return new Response(null, { status: 204 });
    }
    if (path === "/api/keys/key-1/revoke" && method === "POST") {
      keys = [{ ...keys[0], revoked_at: 1 }];
      return new Response(null, { status: 204 });
    }
    if (path === "/api/keys/key-1/unrevoke" && method === "POST") {
      keys = [{ ...keys[0], revoked_at: null }];
      return new Response(null, { status: 204 });
    }
    if (path === "/api/keys") return Response.json({ keys });
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <KeysPage />
    </TooltipProvider>,
  );
  await screen.findByText("No keys created yet.");
  await user.click(screen.getByRole("button", { name: "Create key" }));
  const dialog = await screen.findByRole("dialog");
  await user.type(within(dialog).getByLabelText("Name"), "CI key");
  assert.ok(within(dialog).getByRole("checkbox", { name: "model-c" }));
  await user.type(
    await within(dialog).findByLabelText("Search Providers"),
    "Alpha",
  );
  await user.click(
    within(dialog).getByRole("checkbox", { name: "Alpha (alpha)" }),
  );
  assert.equal(
    within(dialog).queryByRole("checkbox", { name: "model-c" }),
    null,
  );
  await user.type(within(dialog).getByLabelText("Search Models"), "model-a");
  await user.click(within(dialog).getByRole("checkbox", { name: "model-a" }));
  await user.clear(within(dialog).getByLabelText("Search Models"));
  await user.click(within(dialog).getByRole("checkbox", { name: "model-b" }));
  await user.click(
    within(dialog).getByRole("button", { name: /Choose expiration date/ }),
  );
  await user.click(screen.getByRole("button", { name: /previous month/i }));
  const previousMonth = new Date();
  previousMonth.setDate(1);
  previousMonth.setMonth(previousMonth.getMonth() - 1);
  previousMonth.setDate(15);
  const pastDay = document.querySelector<HTMLButtonElement>(
    `[data-slot="calendar"] button[data-day="${previousMonth.toLocaleDateString()}"]`,
  );
  assert.ok(pastDay);
  await user.click(pastDay);
  await user.click(screen.getByRole("button", { name: "Done" }));
  await user.click(within(dialog).getByRole("button", { name: "Create key" }));
  assert.ok(
    await within(dialog).findByText("Expiration must be in the future."),
  );
  assert.equal(creates, 0);
  await user.click(
    within(dialog).getByRole("button", { name: /Choose expiration date/ }),
  );
  await user.click(screen.getByRole("button", { name: /next month/i }));
  await user.click(screen.getByRole("button", { name: /next month/i }));
  const nextMonth = new Date();
  nextMonth.setDate(1);
  nextMonth.setMonth(nextMonth.getMonth() + 1);
  nextMonth.setDate(15);
  const futureDay = document.querySelector<HTMLButtonElement>(
    `[data-slot="calendar"] button[data-day="${nextMonth.toLocaleDateString()}"]`,
  );
  assert.ok(futureDay);
  await user.click(futureDay);
  fireEvent.change(screen.getByLabelText("Time (24-hour)"), {
    target: { value: "99:00" },
  });
  await user.click(screen.getByRole("button", { name: "Done" }));
  await user.click(within(dialog).getByRole("button", { name: "Create key" }));
  assert.ok(
    await within(dialog).findByText("Enter a valid time as HH:MM or HH:MM:SS."),
  );
  assert.equal(creates, 0);
  await user.click(
    within(dialog).getByRole("button", { name: /Choose expiration date/ }),
  );
  fireEvent.change(screen.getByLabelText("Time (24-hour)"), {
    target: { value: "12:00:00" },
  });
  await user.click(screen.getByRole("button", { name: "Done" }));
  await user.click(within(dialog).getByRole("button", { name: "Create key" }));
  await screen.findByText("secret-once");
  await user.click(screen.getByRole("button", { name: "Dismiss" }));
  assert.ok(screen.getByRole("button", { name: /Expires in/ }));
  await user.click(screen.getByRole("button", { name: "Edit CI key" }));
  let edit = await screen.findByRole("dialog");
  await user.click(
    within(edit).getByRole("button", { name: /Choose expiration date/ }),
  );
  fireEvent.change(screen.getByLabelText("Time (24-hour)"), {
    target: { value: "12:30:00" },
  });
  await user.click(screen.getByRole("button", { name: "Done" }));
  await user.click(within(edit).getByRole("button", { name: "Save key" }));
  await waitFor(() => assert.equal(screen.queryByRole("dialog"), null));
  await user.click(screen.getByRole("button", { name: "Edit CI key" }));
  edit = await screen.findByRole("dialog");
  assert.equal(within(edit).getByText("Edit key").tagName, "H2");
  await user.clear(within(edit).getByLabelText("Name"));
  await user.type(within(edit).getByLabelText("Name"), "CI key updated");
  await user.click(
    within(edit).getByRole("button", { name: /Choose expiration date/ }),
  );
  await user.click(screen.getByRole("button", { name: "Clear expiration" }));
  await user.click(within(edit).getByRole("button", { name: "Save key" }));
  await screen.findByText("Never expires");
  await screen.findByText("CI key updated");
  await user.click(
    screen.getByRole("button", { name: "Revoke CI key updated" }),
  );
  const confirmation = await screen.findByRole("dialog");
  await user.click(
    within(confirmation).getByRole("button", { name: "Revoke" }),
  );
  await screen.findByText("Revoked");
  await screen.findByText("Key revoked successfully.");
  const undoButton = screen.getByRole("button", { name: "Undo" });
  await user.click(undoButton);
  await screen.findByText("Key restored successfully.");
  await waitFor(() => assert.equal(screen.queryByText("Revoked"), null));
  assert.ok(screen.getByText("CI key updated"));
  assert.equal(
    (
      screen.getByRole("button", {
        name: "Revoke CI key updated",
      }) as HTMLButtonElement
    ).disabled,
    false,
  );
});

test("catalog failure warns about unrestricted models without blocking key creation", async () => {
  globalThis.fetch = (async (url) => {
    const path = String(url);
    if (path === "/api/providers") return Response.json({ providers: [] });
    if (path === "/api/keys") return Response.json({ keys: [] });
    if (path === "/api/model-catalog")
      return new Response("Unavailable", { status: 503 });
    throw new Error(`Unexpected request: ${path}`);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <KeysPage />
    </TooltipProvider>,
  );
  await user.click(screen.getByRole("button", { name: "Create key" }));
  const dialog = await screen.findByRole("dialog");
  await within(dialog).findByText(/selecting none allows all models/);
  assert.equal(
    (
      within(dialog).getByRole("button", {
        name: "Create key",
      }) as HTMLButtonElement
    ).disabled,
    false,
  );
});
