import { test } from "node:test";
import assert from "node:assert/strict";
import {
  createMemoryHistory,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";
import {
  act,
  within,
  waitFor,
  screen,
  userEvent,
  routeTree,
  mount,
} from "./interactionTestSetup";

test("search and inspect a request in LLM logs", async () => {
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
    virtual_key_id: "vk-1",
    virtual_key_name: "CI key",
    credential_id: "credential-1",
    credential_name: "Primary account",
  };
  const requests: URL[] = [];
  globalThis.fetch = (async (url) => {
    const parsed = new URL(String(url), "http://localhost");
    requests.push(parsed);
    assert.ok(
      parsed.pathname === "/api/logs" || parsed.pathname === "/api/logs/42",
    );
    assert.equal(parsed.searchParams.has("since"), false);
    if (parsed.pathname === "/api/logs/42") return Response.json(log);
    const search = (parsed.searchParams.get("search") ?? "").toLowerCase();
    const logs = [log, { ...log, id: 43, path: "/v1/decisions" }].filter(
      (entry) => entry.path.includes(search),
    );
    return Response.json({
      retention_cutoff: 0,
      page: 1,
      limit: 20,
      total: logs.length,
      logs: logs.map(
        ({ request_body: _request, response_body: _response, ...summary }) =>
          summary,
      ),
      filter_options: {
        providers: ["alpha"],
        models: ["model-a"],
        paths: ["/v1/chat/completions", "/v1/decisions"],
        virtual_keys: [{ id: "vk-1", name: "CI key" }],
        credentials: [{ id: "credential-1", name: "Primary account" }],
      },
      breakdown: { providers: [], models: [] },
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
  const history = createMemoryHistory({ initialEntries: ["/llm-logs"] });
  const router = createRouter({ routeTree, history });
  mount(<RouterProvider router={router} />);
  assert.ok((await screen.findAllByText("Decisions")).length > 0);
  assert.ok(screen.getAllByText("Chat completions").length > 0);
  assert.equal(
    requests.some((url) => url.pathname === "/api/logs/42"),
    false,
  );
  await user.type(screen.getByLabelText("Search request logs"), "decisions");
  assert.ok(
    (await screen.findAllByRole("button", { name: "View request 43" })).length >
      0,
  );
  await waitFor(() => {
    assert.equal(requests.at(-1)?.searchParams.get("search"), "decisions");
    assert.equal(
      screen.queryByRole("button", { name: "View request 42" }),
      null,
    );
  });
  await user.clear(screen.getByLabelText("Search request logs"));
  await user.click(
    (await screen.findAllByRole("button", { name: "View request 42" }))[0],
  );
  const details = await screen.findByRole("dialog");
  assert.ok(within(details).getByText(/alpha \/ model-a/));
  assert.ok(within(details).getByText("CI key"));
  assert.ok(within(details).getByText("Primary account"));
  assert.ok(within(details).getByText("123 ms"));
  assert.ok(await within(details).findByText(/hello/));
  assert.ok(await within(details).findByText(/world/));
  assert.equal(
    requests.filter((url) => url.pathname === "/api/logs/42").length,
    1,
  );
  assert.equal(requests.at(-1)?.pathname, "/api/logs/42");
  await user.keyboard("{Escape}");
  await waitFor(() => assert.equal(screen.queryByRole("dialog"), null));
});

test("LLM log filters load matching requests and reset pagination", async () => {
  const history = createMemoryHistory({ initialEntries: ["/llm-logs"] });
  const requests: URL[] = [];
  const makeLog = (id: number, provider: string) => ({
    id,
    provider,
    model: "test-model",
    path: "/v1/responses",
    status: 200,
    created_at: 1700000000,
    latency_ms: 100,
    input_tokens: null,
    output_tokens: null,
    request_body: "{}",
    response_body: "{}",
    virtual_key_id: null,
    virtual_key_name: null,
    credential_id: null,
    credential_name: null,
  });
  const logs = Array.from({ length: 25 }, (_, index) =>
    makeLog(index + 1, "alpha"),
  );
  const olderMatch = makeLog(99, "beta");
  globalThis.fetch = (async (url) => {
    const parsed = new URL(String(url), "http://localhost");
    requests.push(parsed);
    assert.equal(parsed.pathname, "/api/logs");
    const providers = JSON.parse(
      parsed.searchParams.get("providers") ?? "[]",
    ) as string[];
    const page = Number(parsed.searchParams.get("page") ?? 1);
    const limit = Number(parsed.searchParams.get("limit") ?? 20);
    const search = parsed.searchParams.get("search") ?? "";
    const visible = (providers.includes("beta") ? [olderMatch] : logs).filter(
      (log) => String(log.id).includes(search),
    );
    return Response.json({
      retention_cutoff: 0,
      page,
      limit,
      total: visible.length,
      logs: visible.slice((page - 1) * limit, page * limit),
      filter_options: {
        providers: ["alpha", "beta"],
        models: ["test-model"],
        paths: ["/v1/responses"],
        virtual_keys: [],
        credentials: [],
      },
      breakdown: { providers: [], models: [] },
      summary: {
        requests: visible.length,
        successes: visible.length,
        errors: 0,
        input_tokens: null,
        output_tokens: null,
        avg_latency_ms: 100,
      },
    });
  }) as typeof fetch;
  const user = userEvent.setup();
  mount(<RouterProvider router={createRouter({ routeTree, history })} />);
  await screen.findAllByRole("button", { name: "View request 1" });
  await user.click(screen.getByRole("button", { name: "Go to next page" }));
  assert.ok(
    (await screen.findAllByRole("button", { name: "View request 21" })).length,
  );
  assert.equal(requests.at(-1)?.searchParams.get("page"), "2");
  const filters = screen.getByRole("complementary", {
    name: "Observability filters",
  });
  await user.click(
    within(filters).getByText("Providers", { selector: "summary" }),
  );
  await user.click(within(filters).getByRole("checkbox", { name: "beta" }));
  assert.ok(
    (await screen.findAllByRole("button", { name: "View request 99" })).length,
  );
  assert.equal(screen.queryByRole("button", { name: "View request 21" }), null);
  assert.deepEqual(
    JSON.parse(requests.at(-1)!.searchParams.get("providers")!),
    ["beta"],
  );
  assert.equal(requests.at(-1)!.searchParams.has("since"), false);
  assert.equal(requests.at(-1)?.searchParams.get("page"), "1");
  assert.deepEqual(
    JSON.parse(new URLSearchParams(history.location.search).get("providers")!),
    ["beta"],
  );
  await user.click(within(filters).getByRole("button", { name: "Reset" }));
  assert.ok(
    (await screen.findAllByRole("button", { name: "View request 1" })).length,
  );
  await user.selectOptions(screen.getByLabelText("Rows per page"), "50");
  await waitFor(() =>
    assert.equal(requests.at(-1)?.searchParams.get("limit"), "50"),
  );
  assert.equal(requests.at(-1)?.searchParams.get("page"), "1");
  await user.click(screen.getByRole("button", { name: /Provider \/ model/ }));
  await waitFor(() =>
    assert.match(
      requests.at(-1)?.searchParams.get("sort") ?? "",
      /^model:(asc|desc)$/,
    ),
  );
  assert.equal(requests.at(-1)?.searchParams.get("page"), "1");
});

test("dashboard filters graphs and changes the time range without showing logs", async () => {
  const requests: URL[] = [];
  globalThis.fetch = (async (url) => {
    const parsed = new URL(String(url), "http://localhost");
    requests.push(parsed);
    assert.equal(parsed.pathname, "/api/dashboard");
    return Response.json({
      interval: "hour",
      retention_cutoff: 0,
      buckets: [],
      logs: [],
      filter_options: {
        providers: ["alpha", "beta"],
        models: ["model-a"],
        paths: ["/v1/some/very/long/path/for/request"],
        virtual_keys: [
          { id: "vk-1", name: "CI key" },
          { id: "vk-2", name: "Development" },
        ],
        credentials: [{ id: "credential-1", name: "Primary account" }],
      },
      breakdown: { providers: [], models: [] },
      summary: {
        avg_latency_ms: 0,
        errors: 0,
        input_tokens: null,
        output_tokens: null,
        requests: 0,
        successes: 0,
      },
    });
  }) as typeof fetch;
  const user = userEvent.setup();
  const history = createMemoryHistory({ initialEntries: ["/"] });
  const router = createRouter({ routeTree, history });
  mount(<RouterProvider router={router} />);
  assert.ok(await screen.findByText("Request volume"));
  assert.equal(screen.queryByLabelText("Search request logs"), null);
  const filters = screen.getByRole("complementary", {
    name: "Observability filters",
  });
  await user.click(
    within(filters).getByText("Providers", { selector: "summary" }),
  );
  await user.click(within(filters).getByRole("checkbox", { name: "alpha" }));
  await user.click(within(filters).getByRole("checkbox", { name: "beta" }));
  await waitFor(() =>
    assert.deepEqual(
      JSON.parse(requests.at(-1)!.searchParams.get("providers")!),
      ["alpha", "beta"],
    ),
  );
  await user.click(within(filters).getByRole("checkbox", { name: "Error" }));
  await waitFor(() =>
    assert.deepEqual(
      JSON.parse(requests.at(-1)!.searchParams.get("statuses")!),
      ["error"],
    ),
  );
  assert.deepEqual(
    JSON.parse(new URLSearchParams(history.location.search).get("providers")!),
    ["alpha", "beta"],
  );
  assert.deepEqual(
    JSON.parse(new URLSearchParams(history.location.search).get("statuses")!),
    ["error"],
  );
  await act(async () => {
    history.back();
  });
  await waitFor(() =>
    assert.equal(requests.at(-1)?.searchParams.has("statuses"), false),
  );
  await act(async () => {
    history.forward();
  });
  await waitFor(() =>
    assert.deepEqual(
      JSON.parse(requests.at(-1)!.searchParams.get("statuses")!),
      ["error"],
    ),
  );
  await user.click(
    within(filters).getByText("Virtual keys", { selector: "summary" }),
  );
  await user.type(
    within(filters).getByLabelText("Search Virtual keys"),
    "CI key",
  );
  await user.click(within(filters).getByRole("checkbox", { name: "CI key" }));
  await waitFor(() =>
    assert.deepEqual(
      JSON.parse(requests.at(-1)!.searchParams.get("virtual_key_ids")!),
      ["vk-1"],
    ),
  );
  await user.click(
    within(filters).getByText("Provider credentials", { selector: "summary" }),
  );
  await user.click(
    within(filters).getByRole("checkbox", {
      name: "Primary account",
    }),
  );
  await waitFor(() =>
    assert.deepEqual(
      JSON.parse(requests.at(-1)!.searchParams.get("credential_ids")!),
      ["credential-1"],
    ),
  );
  assert.deepEqual(
    JSON.parse(
      new URLSearchParams(history.location.search).get("virtual_key_ids")!,
    ),
    ["vk-1"],
  );
  assert.deepEqual(
    JSON.parse(
      new URLSearchParams(history.location.search).get("credential_ids")!,
    ),
    ["credential-1"],
  );
  await user.click(
    within(filters).getByText("Request paths", { selector: "summary" }),
  );
  await user.click(
    within(filters).getByRole("checkbox", {
      name: "/v1/some/very/long/path/for/request",
    }),
  );
  await waitFor(() =>
    assert.deepEqual(JSON.parse(requests.at(-1)!.searchParams.get("paths")!), [
      "/v1/some/very/long/path/for/request",
    ]),
  );
  await user.click(
    screen.getByRole("button", { name: /Dashboard time range: Last 24 hours/ }),
  );
  await user.click(await screen.findByRole("button", { name: "Last 7 days" }));
  await waitFor(() => {
    const query = requests.at(-1)!.searchParams;
    assert.ok(
      Math.abs(
        Number(query.get("until")) - Number(query.get("since")) - 7 * 86400,
      ) <= 1,
    );
    assert.deepEqual(JSON.parse(query.get("providers")!), ["alpha", "beta"]);
    assert.deepEqual(JSON.parse(query.get("statuses")!), ["error"]);
    assert.deepEqual(JSON.parse(query.get("virtual_key_ids")!), ["vk-1"]);
    assert.deepEqual(JSON.parse(query.get("credential_ids")!), [
      "credential-1",
    ]);
  });
  await user.click(
    screen.getByRole("button", { name: /Dashboard time range: Last 7 days/ }),
  );
  await user.click(await screen.findByRole("button", { name: "Last hour" }));
  await waitFor(() => {
    const query = requests.at(-1)!.searchParams;
    assert.ok(
      Math.abs(
        Number(query.get("until")) - Number(query.get("since")) - 3600,
      ) <= 1,
    );
  });
  await user.click(
    screen.getByRole("button", { name: /Dashboard time range: Last hour/ }),
  );
  await user.click(await screen.findByRole("button", { name: "Last 6 hours" }));
  await waitFor(() => {
    const query = requests.at(-1)!.searchParams;
    assert.ok(
      Math.abs(
        Number(query.get("until")) - Number(query.get("since")) - 6 * 3600,
      ) <= 1,
    );
  });
  await user.click(within(filters).getByRole("button", { name: "Reset" }));
  await waitFor(() =>
    assert.equal(requests.at(-1)?.searchParams.has("providers"), false),
  );
  assert.equal(requests.at(-1)?.searchParams.has("statuses"), false);
});
