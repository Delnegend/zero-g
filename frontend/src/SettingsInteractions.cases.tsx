import { test } from "node:test";
import assert from "node:assert/strict";
import {
  waitFor,
  screen,
  userEvent,
  SettingsPage,
  mount,
  settings,
} from "./interactionTestSetup";

test("settings page saves model sync intervals in the new card", async () => {
  const bodies: unknown[] = [];
  globalThis.fetch = (async (_url, options) => {
    if (options?.method === "PUT")
      bodies.push(JSON.parse(String(options.body)));
    return Response.json(settings);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<SettingsPage />);
  const pricing = await screen.findByLabelText(
    "Datasheet sync interval (hours)",
  );
  const discovery = await screen.findByLabelText(
    "Model discovery interval (minutes)",
  );
  await user.clear(pricing);
  await user.type(pricing, "12");
  await user.clear(discovery);
  await user.type(discovery, "30");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await screen.findByText("Settings saved.");
  assert.deepEqual(bodies, [
    {
      log_retention_days: 30,
      pricing_interval_hours: 12,
      discovery_interval_minutes: 30,
    },
  ]);
});

test("settings save log retention without provider OAuth settings", async () => {
  const bodies: unknown[] = [];
  globalThis.fetch = (async (_url, options) => {
    if (options?.method === "PUT")
      bodies.push(JSON.parse(String(options.body)));
    return Response.json(settings);
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<SettingsPage />);
  await screen.findByDisplayValue("30");
  assert.equal(screen.queryByLabelText(/Google OAuth/), null);
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await screen.findByText("Settings saved.");
  assert.deepEqual(bodies, [
    {
      log_retention_days: 30,
      pricing_interval_hours: 24,
      discovery_interval_minutes: 60,
    },
  ]);
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
    return Response.json({ ...settings, log_retention_days: 40 });
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<SettingsPage />);
  const retention = await screen.findByLabelText("Log retention (days)");
  await user.clear(retention);
  await user.type(retention, "35");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => assert.equal(bodies.length, 1));
  await user.clear(retention);
  await user.type(retention, "40");
  release(Response.json({ ...settings, log_retention_days: 35 }));
  await screen.findByText("Settings saved.");
  assert.equal((retention as HTMLInputElement).value, "40");
  await user.click(screen.getByRole("button", { name: "Save settings" }));
  await waitFor(() => assert.equal(bodies.length, 2));
  assert.deepEqual(bodies, [
    { ...settings, log_retention_days: 35 },
    { ...settings, log_retention_days: 40 },
  ]);
});
