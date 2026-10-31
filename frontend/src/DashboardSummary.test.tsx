import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { JSDOM } from "jsdom";
import { ObservabilityPage } from "./routes/index";
import { TooltipProvider } from "./components/ui/tooltip";
import type { ObservabilityComparison } from "./server/api/observability";
import type { ObservabilityResponse } from "./server/api/generated/schemas";

const response: ObservabilityResponse = {
  interval: "hour",
  retention_cutoff: 0,
  buckets: [],
  logs: [],
  filter_options: {
    providers: [],
    models: [],
    paths: [],
    virtual_keys: [],
    credentials: [],
  },
  breakdown: { providers: [], models: [] },
  summary: {
    requests: 1234,
    successes: 1200,
    errors: 34,
    input_tokens: 1000,
    output_tokens: 250,
    avg_latency_ms: 123.6,
  },
};

function dashboard(
  summary: ObservabilityResponse["summary"],
  filters = {},
  comparison?: ObservabilityComparison,
) {
  const client = new QueryClient();
  client.setQueryData(
    ["observability", { range: { kind: "preset", value: "24h" }, filters }],
    { ...response, summary, comparison },
  );
  const html = renderToStaticMarkup(
    <QueryClientProvider client={client}>
      <TooltipProvider>
        <ObservabilityPage filters={filters} onFiltersChange={() => {}} />
      </TooltipProvider>
    </QueryClientProvider>,
  );
  const document = new JSDOM(html).window.document;
  const section = document.querySelector('[aria-label="Request summary"]');
  assert.ok(section);
  return section;
}

function cardText(section: Element, label: string) {
  const heading = Array.from(section.querySelectorAll("h2")).find(
    (element) => element.textContent === label,
  );
  assert.ok(heading);
  return heading.parentElement!.parentElement!.textContent;
}

test("dashboard summary shows the current filtered totals and details", () => {
  const section = dashboard(response.summary, { providers: ["alpha"] });
  assert.equal(section.querySelectorAll("h2").length, 4);
  assert.match(cardText(section, "Total requests") ?? "", /1,234/);
  assert.match(cardText(section, "Successful") ?? "", /1,200.*34/);
  assert.match(cardText(section, "Tokens") ?? "", /1,250.*1,000.*250/);
  assert.match(cardText(section, "Average latency") ?? "", /124 ms/);
  assert.equal(section.querySelector("svg"), null);
});

test("dashboard does not invent token or latency values", () => {
  const section = dashboard({
    requests: 0,
    successes: 0,
    errors: 0,
    input_tokens: null,
    output_tokens: 25,
    avg_latency_ms: 0,
  });
  assert.match(cardText(section, "Tokens") ?? "", /Unknown.*Unknown.*25/);
  assert.match(cardText(section, "Average latency") ?? "", /Unknown/);
});

test("summary trends compare the same metrics and treat lower latency as an improvement", () => {
  const previous = {
    ...response,
    summary: {
      ...response.summary,
      requests: 617,
      successes: 1500,
      input_tokens: 500,
      output_tokens: 125,
      avg_latency_ms: 200,
    },
  };
  const section = dashboard(
    response.summary,
    {},
    {
      data: previous,
      currentSince: 604800,
      currentUntil: 1209600,
      previousSince: -1,
      previousUntil: 604799,
      retentionCutoff: 0,
    },
  );
  const trend = (label: string) =>
    section.querySelector<HTMLButtonElement>(
      `button[aria-label^="${label}:"]`,
    )!;
  assert.match(
    trend("Total requests").getAttribute("aria-label")!,
    /increased 100%.*previous 7 days/,
  );
  assert.match(
    trend("Successful").getAttribute("aria-label")!,
    /decreased 20%/,
  );
  assert.match(trend("Tokens").getAttribute("aria-label")!, /increased 100%/);
  assert.match(
    trend("Average latency").getAttribute("aria-label")!,
    /decreased 38.2%/,
  );
  assert.match(trend("Total requests").className, /text-green-700/);
  assert.match(trend("Successful").className, /text-red-700/);
  assert.match(trend("Average latency").className, /text-green-700/);
});

test("summary trends do not invent percentages for missing or zero baselines", () => {
  const previous = {
    ...response,
    summary: { ...response.summary, requests: 0, input_tokens: null },
  };
  const section = dashboard(
    response.summary,
    {},
    {
      data: previous,
      currentSince: 3600,
      currentUntil: 7200,
      previousSince: -1,
      previousUntil: 3599,
      retentionCutoff: 0,
    },
  );
  const requestTrend = section.querySelector(
    'button[aria-label^="Total requests:"]',
  );
  assert.match(requestTrend?.textContent ?? "", /New/);
  assert.match(
    requestTrend?.getAttribute("aria-label") ?? "",
    /previous 1 hour was zero/,
  );
  assert.equal(section.querySelector('button[aria-label^="Tokens:"]'), null);
  assert.equal(
    section.querySelector('button[aria-label^="Average latency:"]'),
    null,
  );
});
