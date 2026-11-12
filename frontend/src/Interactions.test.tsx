import { test } from "node:test";
import assert from "node:assert/strict";
import {
  waitFor,
  screen,
  userEvent,
  ProviderCard,
  ProviderForm,
  TooltipProvider,
  mount,
} from "./interactionTestSetup";
import "./SettingsInteractions.cases";
import "./CredentialInteractions.cases";
import "./KeyInteractions.cases";
import "./ObservabilityInteractions.cases";
import "./CatalogInteractions.cases";

test("built-in provider names remain visible without an extra label", async () => {
  globalThis.fetch = (async () =>
    Response.json({ credentials: [] })) as typeof fetch;
  const kind = {
    value: "zen",
    label: null,
    api_key: true,
    oauth: false,
    fixed_id: "zen",
    fixed_name: "OpenCode Zen",
  };
  const view = mount(
    <TooltipProvider>
      <ProviderCard
        provider={{
          id: "zen",
          name: "OpenCode Zen",
          kind: "zen",
          base_url: "",
          keyless: false,
        }}
        kind={kind}
        onEdit={() => {}}
        onDelete={() => {}}
        deleting={false}
      />
    </TooltipProvider>,
  );
  assert.ok(screen.getByRole("heading", { name: "OpenCode Zen" }));
  assert.equal(screen.queryByText(/· zen/), null);
  await screen.findByText("No credentials configured.");
  view.unmount();
  mount(<ProviderForm kinds={[kind]} onDone={() => {}} onCancel={() => {}} />);
  assert.ok(
    screen
      .getByRole("combobox", { name: "Kind" })
      .textContent?.includes("OpenCode Zen"),
  );
});

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
            keyless: false,
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
