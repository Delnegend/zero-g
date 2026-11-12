import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
function parseDocument(html: string) {
  document.body.innerHTML = html;
  return document;
}
import { ObservabilityCharts } from "./ObservabilityCharts";
import type { ObservabilityResponse } from "@/server/api/generated/schemas";

const bucket = (
  start: number,
  requests: number,
  input_tokens: number | null,
) => ({
  start,
  requests,
  successes: requests,
  errors: 0,
  input_tokens,
  output_tokens: null,
  avg_latency_ms: 12.3456,
  by_model: { model: requests },
  by_provider: { provider: { requests, input_tokens, output_tokens: null } },
});
const breakdown = {
  providers: [{ name: "provider", requests: 2 }],
  models: [{ name: "model", requests: 2 }],
};
const previous: ObservabilityResponse = {
  interval: "hour",
  buckets: [bucket(0, 1, null), bucket(7200, 1, 7)],
  breakdown,
  retention_cutoff: 0,
  logs: [],
  filter_options: {
    providers: [],
    models: [],
    paths: [],
    virtual_keys: [],
    credentials: [],
  },
  summary: {
    requests: 2,
    successes: 2,
    errors: 0,
    input_tokens: 7,
    output_tokens: null,
    avg_latency_ms: 12.3456,
  },
};
const comparison = {
  data: previous,
  previousSince: 0,
  previousUntil: 10799,
  currentSince: 10800,
  currentUntil: 21599,
  retentionCutoff: 0,
};

test("comparison preserves sparse bucket positions and unknown tokens", () => {
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="hour"
      buckets={[bucket(10800, 2, null), bucket(18000, 2, 4)]}
      breakdown={breakdown}
      comparison={comparison}
    />,
  );
  assert.match(html, /not reported/);
  assert.match(html, /1970-01-01 03:00 UTC/);
  assert.match(html, /1970-01-01 05:00 UTC/);
  assert.match(html, /12\.3456 ms/);
  assert.match(html, /Request volume: 100% higher/);
});

test("token usage combines input and output while preserving totals, trends and missing values", () => {
  const current = [
    { ...bucket(10800, 1, 4), output_tokens: 0 },
    { ...bucket(14400, 1, null), output_tokens: 6 },
  ];
  const prior = [
    { ...bucket(0, 1, 7), output_tokens: null },
    { ...bucket(3600, 1, null), output_tokens: 9 },
  ];
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="hour"
      buckets={current}
      breakdown={breakdown}
      comparison={{ ...comparison, data: { ...previous, buckets: prior } }}
    />,
  );
  const document = parseDocument(html);
  const charts = [...document.querySelectorAll(".request-chart")];
  assert.equal(charts.length, 6);
  const panel = charts[2].parentElement!;
  assert.match(panel.textContent ?? "", /Input: 4; output: 6 tokens/);
  const trends = [...panel.querySelectorAll("button")].map(
    (button) => button.getAttribute("aria-label") ?? "",
  );
  assert.ok(
    trends.some((label) =>
      /Input tokens: 42.86% lower.*Previous 3 hours: 7 tokens/.test(label),
    ),
  );
  assert.ok(
    trends.some((label) =>
      /Output tokens: 33.33% lower.*Previous 3 hours: 9 tokens/.test(label),
    ),
  );
  assert.match(
    panel.querySelector('[aria-label="Token usage data"]')?.textContent ?? "",
    /Input: 4 tokens; output: 0 tokens.*Input: not reported; output: 6 tokens/,
  );
  assert.equal(charts[2].querySelectorAll("rect[fill]").length, 3);
  assert.equal(charts[2].querySelectorAll("g.ts-chart__line").length, 0);
});

test("unavailable and zero-baseline comparisons are distinguished", () => {
  const props = {
    interval: "hour" as const,
    buckets: [bucket(10800, 2, null)],
    breakdown,
  };
  const unavailable = renderToStaticMarkup(
    <ObservabilityCharts
      {...props}
      comparison={{ ...comparison, data: null }}
    />,
  );
  assert.match(unavailable, /earlier period could not be loaded/);
  assert.doesNotMatch(unavailable, /Previous 3 hours: 0 requests/);
  const empty = {
    ...previous,
    buckets: [],
    breakdown: { providers: [], models: [] },
  };
  const zero = renderToStaticMarkup(
    <ObservabilityCharts
      {...props}
      comparison={{ ...comparison, data: empty }}
    />,
  );
  assert.match(zero, /Request volume: New.*Previous 3 hours: 0 requests/);
  assert.match(zero, /not reported/);
});

test("sparse and single-bucket charts retain the selected time range", () => {
  const render = (starts: number[]) => {
    const html = renderToStaticMarkup(
      <ObservabilityCharts
        interval="hour"
        buckets={starts.map((start) => bucket(start, 1, 5))}
        breakdown={breakdown}
        comparison={{ ...comparison, currentSince: 0, currentUntil: 36000 }}
      />,
    );
    return parseDocument(html).querySelector(".request-chart")!;
  };
  const two = [...render([3600, 7200]).querySelectorAll("rect[fill]")];
  const first = Number(two[0].getAttribute("x"));
  const second = Number(two[1].getAttribute("x"));
  const single = render([7200]);
  const singleX = Number(single.querySelector("rect[fill]")!.getAttribute("x"));
  assert.ok(second > first);
  assert.ok(Math.abs(singleX - second) < 1);
  assert.match(single.textContent ?? "", /01-01 0[89]:/);
});

test("seven-day current points render their matching bars cleanly", () => {
  const day = 86400;
  const current = [0, 3, 6].map((i) => ({
    ...bucket((i + 7) * day, i + 3, i + 10),
    successes: i + 2,
    errors: 1,
  }));
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="day"
      buckets={current}
      breakdown={breakdown}
      comparison={{
        data: { ...previous, interval: "day", buckets: [] },
        currentSince: 7 * day,
        currentUntil: 14 * day - 1,
        previousSince: 0,
        previousUntil: 7 * day - 1,
        retentionCutoff: 0,
      }}
    />,
  );
  const document = parseDocument(html);
  const chart = document.querySelector(".request-chart");
  assert.ok(chart);
  const bars = [...chart.querySelectorAll("rect[fill]")];
  assert.equal(bars.length, 6);
  assert.equal(chart.querySelectorAll("g.ts-chart__line").length, 0);
  const firstSeries = bars
    .slice(0, 3)
    .map((bar) => Number(bar.getAttribute("x")));
  const secondSeries = bars
    .slice(3)
    .map((bar) => Number(bar.getAttribute("x")));
  assert.deepEqual(firstSeries, secondSeries);
});

test("charts render bar series for current period without dashed line series", () => {
  const day = 86400;
  const current = [8, 11, 13].map((i) => bucket(i * day, i, i));
  const prior = [0, 2, 4, 6].map((i) => bucket(i * day, i + 1, i));
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="day"
      buckets={current}
      breakdown={breakdown}
      comparison={{
        data: { ...previous, interval: "day", buckets: prior },
        currentSince: 7 * day + 3600,
        currentUntil: 14 * day - 1,
        previousSince: 3600,
        previousUntil: 7 * day - 1,
        retentionCutoff: 0,
      }}
    />,
  );
  const document = parseDocument(html);
  const chart = document.querySelector(".request-chart");
  assert.ok(chart);
  const lines = chart.querySelectorAll("g.ts-chart__line");
  assert.equal(lines.length, 0);
  assert.equal(
    document.querySelector(
      '[aria-label="Request volume previous period data"]',
    ),
    null,
  );
});

test("model usage stacks only current models without previous period content", () => {
  const current = { ...bucket(86400, 8, 100), by_model: { alpha: 3, beta: 5 } };
  const prior = { ...bucket(0, 8, 100), by_model: { old: 8 } };
  const models = {
    providers: [],
    models: [
      { name: "alpha", requests: 3 },
      { name: "beta", requests: 5 },
    ],
  };
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="day"
      buckets={[current]}
      breakdown={models}
      comparison={{
        data: {
          ...previous,
          interval: "day",
          buckets: [prior, { ...prior, start: 3 * 86400 }],
          breakdown: { ...models, models: [{ name: "old", requests: 8 }] },
        },
        currentSince: 86400,
        currentUntil: 172799,
        previousSince: 0,
        previousUntil: 86399,
        retentionCutoff: 0,
      }}
    />,
  );
  const document = parseDocument(html);
  const panels = [...document.querySelectorAll(".request-chart")];
  assert.equal(panels.length, 6);
  const chart = panels[3];
  assert.ok(chart);
  const bars = [...chart.querySelectorAll("rect[fill]")];
  assert.equal(bars.length, 2);
  assert.equal(bars[0].getAttribute("x"), bars[1].getAttribute("x"));
  const lowerTop = Number(bars[0].getAttribute("y"));
  const upperBottom =
    Number(bars[1].getAttribute("y")) + Number(bars[1].getAttribute("height"));
  assert.ok(Math.abs(lowerTop - upperBottom) < 0.02);
  assert.equal(chart.querySelectorAll("g.ts-chart__line").length, 0);
  const panel = chart.parentElement;
  assert.ok(panel);
  assert.doesNotMatch(panel.textContent ?? "", /Previous|\bold\b/i);
  assert.equal(
    panel.querySelector('[aria-label="Model usage previous period data"]'),
    null,
  );
  assert.doesNotMatch(chart.textContent ?? "", /1970-01-05/);
});

test("provider charts stack requests and reported token usage by time", () => {
  const current: ObservabilityResponse["buckets"] = [
    {
      ...bucket(3600, 3, 100),
      by_provider: {
        alpha: { requests: 2, input_tokens: 80, output_tokens: 20 },
        beta: { requests: 1, input_tokens: null, output_tokens: null },
      },
    },
    {
      ...bucket(7200, 1, 0),
      by_provider: { beta: { requests: 1, input_tokens: 0, output_tokens: 5 } },
    },
  ];
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="hour"
      buckets={current}
      breakdown={{
        providers: [
          { name: "alpha", requests: 2 },
          { name: "beta", requests: 2 },
        ],
        models: [],
      }}
    />,
  );
  const document = parseDocument(html);
  const requests = document.querySelector(
    '[aria-label="Requests by provider data"]',
  );
  const tokens = document.querySelector(
    '[aria-label="Token usage by provider data"]',
  );
  assert.match(
    requests?.textContent ?? "",
    /01:00 UTC: alpha: 2, beta: 1.*02:00 UTC: alpha: 0, beta: 1/,
  );
  assert.match(
    tokens?.textContent ?? "",
    /01:00 UTC: alpha: 100, beta: not reported.*02:00 UTC: alpha: not reported, beta: 5/,
  );
  const panel = requests?.parentElement;
  assert.match(panel?.textContent ?? "", /alpha/);
  assert.match(panel?.textContent ?? "", /beta/);
});
