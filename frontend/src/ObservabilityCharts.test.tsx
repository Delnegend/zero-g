import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { JSDOM } from "jsdom";
import { ObservabilityCharts } from "./ObservabilityCharts";
import type { ObservabilityResponse } from "./server/api/generated/schemas";

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

test("comparison preserves sparse bucket positions, exact previous values and unknown tokens", () => {
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="hour"
      buckets={[bucket(10800, 2, null), bucket(18000, 2, 4)]}
      breakdown={breakdown}
      comparison={comparison}
    />,
  );
  assert.match(html, /not reported/);
  assert.match(
    html,
    /1970-01-01 02:00 UTC \(aligned with 1970-01-01 05:00 UTC\)/,
  );
  assert.match(
    html,
    /1970-01-01 05:00 UTC \(aligned with 1970-01-01 05:00 UTC\)/,
  );
  assert.match(html, /12\.3456 ms/);
  assert.match(html, /Request volume: 100% higher/);
  assert.match(html, /Previous period data/i);
});

test("input and output panels show independent reported totals and gaps", () => {
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
  const document = new JSDOM(html).window.document;
  const charts = [...document.querySelectorAll(".request-chart")];
  assert.equal(charts.length, 6);
  const input = charts[2].parentElement!;
  const output = charts[3].parentElement!;
  assert.match(
    input.querySelector("h2")?.parentElement?.parentElement?.textContent ?? "",
    /4 tokens.*42.86%/,
  );
  assert.match(
    input.querySelector("button")?.getAttribute("aria-label") ?? "",
    /42.86% lower.*Previous 3 hours: 7 tokens/,
  );
  assert.match(
    output.querySelector("h2")?.parentElement?.parentElement?.textContent ?? "",
    /6 tokens.*33.33%/,
  );
  assert.match(
    output.querySelector("button")?.getAttribute("aria-label") ?? "",
    /33.33% lower.*Previous 3 hours: 9 tokens/,
  );
  assert.match(
    input.querySelector('[aria-label="Input tokens data"]')?.textContent ?? "",
    /4 tokens.*not reported/,
  );
  assert.match(
    output.querySelector('[aria-label="Output tokens data"]')?.textContent ??
      "",
    /0 tokens.*6 tokens/,
  );
  assert.match(
    input.querySelector('[aria-label="Input tokens previous period data"]')
      ?.textContent ?? "",
    /7 tokens.*not reported/,
  );
  assert.match(
    output.querySelector('[aria-label="Output tokens previous period data"]')
      ?.textContent ?? "",
    /not reported.*9 tokens/,
  );
  assert.equal(charts[2].querySelectorAll("rect[fill]").length, 1);
  assert.equal(charts[3].querySelectorAll("rect[fill]").length, 2);
  assert.equal(
    charts[2].querySelectorAll("g.ts-chart__line circle[fill]").length,
    1,
  );
  assert.equal(
    charts[3].querySelectorAll("g.ts-chart__line circle[fill]").length,
    1,
  );
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

test("seven-day previous points overlay their matching bars despite missing current days", () => {
  const day = 86400;
  const current = [0, 3, 6].map((i) => ({
    ...bucket((i + 7) * day, i + 3, i + 10),
    successes: i + 2,
    errors: 1,
  }));
  const prior = Array.from({ length: 7 }, (_, i) =>
    bucket(i * day, i + 1, i + 5),
  );
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="day"
      buckets={current}
      breakdown={breakdown}
      comparison={{
        data: { ...previous, interval: "day", buckets: prior },
        currentSince: 7 * day,
        currentUntil: 14 * day - 1,
        previousSince: 0,
        previousUntil: 7 * day - 1,
        retentionCutoff: 0,
      }}
    />,
  );
  const document = new JSDOM(html).window.document;
  const chart = document.querySelector(".request-chart");
  assert.ok(chart);
  const bars = [...chart.querySelectorAll("rect[fill]")];
  const points = [...chart.querySelectorAll("circle[fill]")];
  assert.equal(bars.length, 6);
  for (const bar of bars) {
    const center =
      Number(bar.getAttribute("x")) + Number(bar.getAttribute("width")) / 2;
    const matching = points.filter(
      (point) => point.getAttribute("fill") === bar.getAttribute("fill"),
    );
    assert.ok(
      matching.some(
        (point) => Math.abs(Number(point.getAttribute("cx")) - center) < 0.02,
      ),
    );
  }
  const firstSeries = bars
    .slice(0, 3)
    .map((bar) => Number(bar.getAttribute("x")));
  const secondSeries = bars
    .slice(3)
    .map((bar) => Number(bar.getAttribute("x")));
  assert.deepEqual(firstSeries, secondSeries);
  for (let index = 0; index < 3; index++) {
    const success = bars[index];
    const error = bars[index + 3];
    const successTop = Number(success.getAttribute("y"));
    const errorBottom =
      Number(error.getAttribute("y")) + Number(error.getAttribute("height"));
    assert.ok(Math.abs(successTop - errorBottom) < 0.02);
  }
});

test("previous activity overlays the first current bars while preserving gaps", () => {
  const day = 86400;
  const current = [7, 8, 9].map((i) => bucket(i * day, i, i));
  const prior = [4, 6].map((i) => bucket(i * day, i, i));
  const html = renderToStaticMarkup(
    <ObservabilityCharts
      interval="day"
      buckets={current}
      breakdown={breakdown}
      comparison={{
        data: { ...previous, interval: "day", buckets: prior },
        currentSince: 7 * day,
        currentUntil: 14 * day - 1,
        previousSince: 0,
        previousUntil: 7 * day - 1,
        retentionCutoff: 0,
      }}
    />,
  );
  const document = new JSDOM(html).window.document;
  const chart = document.querySelector(".request-chart");
  assert.ok(chart);
  const bars = [...chart.querySelectorAll("rect[fill]")].slice(0, 3);
  const points = [
    ...chart.querySelectorAll("g.ts-chart__line circle[fill]"),
  ].filter(
    (point) => point.getAttribute("fill") === bars[0].getAttribute("fill"),
  );
  assert.equal(points.length, 2);
  for (const [point, index] of [
    [points[0], 0],
    [points[1], 2],
  ] as const) {
    const bar = bars[index];
    const center =
      Number(bar.getAttribute("x")) + Number(bar.getAttribute("width")) / 2;
    assert.ok(Math.abs(Number(point.getAttribute("cx")) - center) < 0.02);
  }
  const previousRows = document.querySelector(
    '[aria-label="Request volume previous period data"]',
  );
  assert.ok(
    previousRows?.textContent?.includes(
      "1970-01-05 UTC (aligned with 1970-01-08 UTC)",
    ),
  );
  assert.ok(
    previousRows?.textContent?.includes(
      "1970-01-07 UTC (aligned with 1970-01-10 UTC)",
    ),
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
  const document = new JSDOM(html).window.document;
  const panels = [...document.querySelectorAll(".request-chart")];
  assert.equal(panels.length, 6);
  const chart = panels[4];
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
