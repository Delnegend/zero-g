import { barY, defineChart } from "@tanstack/charts";
import { motion } from "@tanstack/charts/motion";
import { Chart } from "@tanstack/charts/react/core";
import { scaleBand } from "@tanstack/charts/scales/band";
import { scaleLinear } from "@tanstack/charts/scales/linear";
import { tooltip } from "@tanstack/charts/tooltip";
import { Minus, TrendingDown, TrendingUp } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { Card } from "./ui/card";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "./ui/tooltip";
import type { ObservabilityComparison } from "@/server/api/observability";

type Interval = "hour" | "day" | "month" | "year";
type Bucket = {
  start: number;
  requests: number;
  successes: number;
  errors: number;
  input_tokens: number | null;
  output_tokens: number | null;
  avg_latency_ms: number;
  by_model: Record<string, number>;
  by_provider: Record<
    string,
    {
      requests: number;
      input_tokens: number | null;
      output_tokens: number | null;
    }
  >;
};
type Breakdown = { name: string; requests: number }[];
type Point = {
  label: string;
  value: number;
  start?: number;
};
type Series = { name: string; color: string; points: Point[] };
type Row = { label: string; value: string };
type Comparison = {
  current: number | null;
  previous: number | null;
  detail: string;
};

function formatChartPeriod(label: string) {
  return `Current: ${label} UTC`;
}

function GraphTrend({
  title,
  comparison,
}: {
  title: string;
  comparison: Comparison;
}) {
  const { current, previous, detail } = comparison;
  const change =
    current === null || previous === null
      ? null
      : previous === 0
        ? current === 0
          ? 0
          : null
        : ((current - previous) / previous) * 100;
  const isNew = current !== null && current > 0 && previous === 0;
  const value = isNew
    ? "New"
    : change === null
      ? "-"
      : change !== 0 && Math.abs(change) < 0.01
        ? "<0.01%"
        : `${number.format(Math.abs(change))}%`;
  const direction =
    change === null || change === 0 ? "" : change > 0 ? "higher" : "lower";
  const Icon =
    change === null || change === 0
      ? Minus
      : change > 0
        ? TrendingUp
        : TrendingDown;
  return (
    <Tooltip>
      <TooltipTrigger
        type="button"
        aria-label={`${title}: ${value}${direction ? ` ${direction}` : ""}. ${detail}`}
        className="inline-flex shrink-0 items-center gap-1 text-xs text-muted-foreground"
      >
        <Icon aria-hidden="true" className="size-4" />
        {value}
      </TooltipTrigger>
      <TooltipContent>{detail}</TooltipContent>
    </Tooltip>
  );
}

const chartRenderer = motion({
  transition: { type: "spring", stiffness: 170, damping: 18, mass: 1 },
});
const number = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const compact = new Intl.NumberFormat(undefined, {
  notation: "compact",
  maximumFractionDigits: 1,
});
const modelColors = ["#3b82f6", "#a855f7", "#10b981", "#f59e0b", "#64748b"];

function bucketLabel(start: number, interval: Interval) {
  const iso = new Date(start * 1000).toISOString();
  return interval === "hour"
    ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}`
    : iso.slice(0, interval === "day" ? 10 : interval === "month" ? 7 : 4);
}

function GraphPanel({
  title,
  description,
  unit,
  series,
  alternative,
  domain,
  comparison,
  comparisons,
  summary,
  timeSeries = false,
  timeRange,
  stacked = false,
  emptyMessage = "No requests to chart.",
}: {
  title: string;
  description: string;
  unit: string;
  series: Series[];
  alternative: Row[];
  domain: string[];
  comparison?: Comparison;
  comparisons?: { title: string; value: Comparison }[];
  summary: string;
  timeSeries?: boolean;
  timeRange?: readonly [number, number];
  stacked?: boolean;
  emptyMessage?: string;
}) {
  const [desktop, setDesktop] = useState(
    () =>
      typeof window !== "undefined" &&
      window.matchMedia("(min-width: 640px)").matches,
  );
  useEffect(() => {
    const media = window.matchMedia("(min-width: 640px)");
    const update = () => setDesktop(media.matches);
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);
  const pointCount = series.reduce(
    (total, item) => total + item.points.length,
    0,
  );
  const definition = useMemo(() => {
    // Give each grouped series its own band so its previous line and bar share
    // the same horizontal position.
    const grouped = !stacked && series.length > 1 && !timeSeries;
    const slot = (label: string, name: string) =>
      grouped ? JSON.stringify([label, name]) : label;
    const labels = new Map(
      domain.flatMap((label) =>
        series.map((item) => [slot(label, item.name), label] as const),
      ),
    );
    const chartDomain = grouped ? [...labels.keys()] : domain;
    const bars = series.flatMap((item) =>
      item.points.map((point) => ({
        ...point,
        series: item.name,
        color: item.color,
      })),
    );
    const timeStarts = timeSeries
      ? [
          ...new Set(
            bars
              .map((point) => point.start)
              .filter((v): v is number => v !== undefined),
          ),
        ]
      : [];
    const timeMin =
      timeRange?.[0] ?? (timeStarts.length ? Math.min(...timeStarts) : 0);
    const timeMax =
      timeRange?.[1] ?? (timeStarts.length ? Math.max(...timeStarts) : 1);
    const timeLabelFor = (value: number) => {
      const date = new Date(value * 1000).toISOString();
      return date.includes("T")
        ? `${date.slice(0, 10)} ${date.slice(11, 16)}`
        : date.slice(0, 10);
    };
    return defineChart({
      marks: [
        barY(bars, {
          x: timeSeries
            ? (point) =>
                Math.max(timeMin, Math.min(timeMax, point.start ?? timeMin))
            : (point) => slot(point.label, point.series),
          y: "value",
          z: "series",
          ...(stacked ? { layout: { type: "stack" as const } } : {}),
          fill: (point) => point.color,
          fillOpacity: 0.55,
          maxThickness: 30,
        }),
      ],
      scales: {
        x: timeSeries
          ? {
              scale: scaleLinear().domain([
                timeMin,
                timeMax > timeMin ? timeMax : timeMin + 1,
              ]),
              axis: {
                ticks: {
                  count: 6,
                  format: (value) => {
                    const label = timeLabelFor(Number(value));
                    return label.includes(" ") ? label.slice(5) : label;
                  },
                },
                tickLabels: { fontSize: 10, thin: true },
              },
            }
          : {
              scale: scaleBand().domain(chartDomain).padding(0.18),
              axis: {
                ticks: {
                  spacing: 60,
                  format: (value) => {
                    const label = labels.get(String(value)) ?? String(value);
                    return label.length > 12 ? `${label.slice(0, 11)}…` : label;
                  },
                },
                tickLabels: { fontSize: 10, thin: true },
              },
            },
        y: {
          scale: scaleLinear,
          nice: true,
          grid: true,
          axis: {
            ticks: {
              count: 4,
              format: (value) => compact.format(Number(value)),
            },
            tickLabels: { fontSize: 10 },
          },
        },
      },
      tooltip: {
        use: tooltip,
        items: [
          ...(timeSeries
            ? [
                {
                  id: "period",
                  label: "Period",
                  text: (point: { datum: unknown; xValue: unknown }) => {
                    const datum = point.datum as Point;
                    return formatChartPeriod(datum.label);
                  },
                },
              ]
            : [{ channel: "x" as const, label: "Category" }]),
          ...(stacked
            ? [
                {
                  id: "series",
                  label: "Series",
                  text: (point: { datum: unknown }) => {
                    const datum = point.datum;
                    return datum &&
                      typeof datum === "object" &&
                      "series" in datum &&
                      typeof datum.series === "string"
                      ? datum.series
                      : "";
                  },
                },
              ]
            : []),
          {
            channel: "y",
            label: unit,
          },
        ],
      },
    });
  }, [series, unit, domain, timeSeries, stacked, timeRange]);

  return (
    <Card className="flex min-w-0 flex-col gap-0 p-3 shadow-none sm:p-5">
      <div className="flex min-h-8 min-w-0 items-start justify-between gap-2">
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-2">
          <h2 className="text-sm font-medium">{title}</h2>
          <span className="text-xs text-muted-foreground">{summary}</span>
        </div>
        {comparison && <GraphTrend title={title} comparison={comparison} />}
        {comparisons && (
          <div className="flex flex-wrap justify-end">
            {comparisons.map(({ title: label, value }) => (
              <span
                key={label}
                className="inline-flex items-center gap-1 text-xs text-muted-foreground"
              >
                {label}
                <GraphTrend title={label} comparison={value} />
              </span>
            ))}
          </div>
        )}
      </div>
      <div className="request-chart h-[135px] w-full min-w-0 shrink-0 overflow-hidden text-foreground sm:h-[205px]">
        {pointCount === 0 ? (
          <p className="pt-8 text-center text-xs text-muted-foreground">
            {emptyMessage}
          </p>
        ) : (
          <Chart
            definition={definition}
            renderer={chartRenderer}
            height={desktop ? 205 : 135}
            ariaLabel={title}
            ariaDescription={`${description} Exact values follow the chart.`}
          />
        )}
      </div>
      {series.length > 0 && (
        <div className="flex min-w-0 shrink-0 flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted-foreground">
          {series.map((item) => (
            <span
              key={item.name}
              className="inline-flex max-w-full items-center gap-1"
              title={item.name}
            >
              <span
                aria-hidden="true"
                className="size-2 shrink-0 rounded-full"
                style={{ backgroundColor: item.color }}
              />
              <span className="max-w-24 truncate">{item.name}</span>
            </span>
          ))}
        </div>
      )}
      {alternative.length > 0 && (
        <ul className="sr-only" aria-label={`${title} data`}>
          {alternative.map((row, index) => (
            <li key={`${row.label}-${index}`}>
              {row.label}: {row.value}
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

export function ObservabilityCharts({
  interval,
  buckets,
  breakdown,
  comparison,
}: {
  interval: Interval;
  buckets: Bucket[];
  breakdown: { providers: Breakdown; models: Breakdown };
  comparison?: ObservabilityComparison;
}) {
  const periods = [...buckets]
    .sort((a, b) => a.start - b.start)
    .map((bucket) => ({
      ...bucket,
      label: bucketLabel(bucket.start, interval),
      date: bucketLabel(bucket.start, interval),
    }));
  const domain = periods.map((p) => p.label);
  const timeRange = comparison
    ? ([comparison.currentSince, comparison.currentUntil] as const)
    : undefined;
  const rows = (
    items: typeof periods,
    value: (bucket: (typeof periods)[number]) => string,
  ) =>
    items.map((bucket) => ({
      label: `${bucket.date} UTC`,
      value: value(bucket),
    }));
  const periodRows = (value: (bucket: (typeof periods)[number]) => string) =>
    rows(periods, value);
  const previousData = comparison?.data ?? null;
  const previousRequests = previousData
    ? previousData.buckets.reduce((sum, p) => sum + p.requests, 0)
    : null;
  const totalTokens = (
    items: Bucket[],
    field: "input_tokens" | "output_tokens",
  ) => {
    const values = items
      .map((p) => p[field])
      .filter((value): value is number => value !== null);
    return values.length ? values.reduce((sum, value) => sum + value, 0) : null;
  };
  const averageLatency = (items: Bucket[]) => {
    const count = items.reduce((sum, p) => sum + p.requests, 0);
    return count
      ? items.reduce((sum, p) => sum + p.avg_latency_ms * p.requests, 0) / count
      : null;
  };
  const unavailable = !comparison
    ? "There is no earlier period to compare."
    : !previousData
      ? "The earlier period could not be loaded or is outside the saved history."
      : null;
  const seconds = comparison
    ? comparison.previousUntil - comparison.previousSince
    : 0;
  const periodUnit =
    seconds >= 86400 ? "day" : seconds >= 3600 ? "hour" : "minute";
  const periodLength =
    seconds /
    (periodUnit === "day" ? 86400 : periodUnit === "hour" ? 3600 : 60);
  const period =
    periodLength > 0
      ? `Previous ${number.format(periodLength)} ${periodUnit}${periodLength === 1 ? "" : "s"}`
      : "Previous period";
  const compare = (
    current: number | null,
    previous: number | null,
    unit: string,
  ): Comparison => ({
    current,
    previous: unavailable ? null : previous,
    detail: `${unavailable ?? (previous === null ? `${period}: not reported.` : `${period}: ${number.format(previous)} ${unit}.${current === null ? " The current value was not reported." : ""}`)}${
      previousData ? " Older data may be missing." : ""
    }`,
  });
  const requestComparison = compare(
    periods.reduce((sum, p) => sum + p.requests, 0),
    previousRequests,
    "requests",
  );
  const volumeRows = (p: (typeof periods)[number]) =>
    `${number.format(p.requests)} requests; ${number.format(p.successes)} successful, ${number.format(p.errors)} errors`;
  const requests = periods.reduce((sum, p) => sum + p.requests, 0);
  const tokenTotal = (field: "input_tokens" | "output_tokens") => {
    const reported = periods
      .map((p) => p[field])
      .filter((value): value is number => value !== null);
    return reported.length
      ? number.format(reported.reduce((sum, value) => sum + value, 0))
      : "not reported";
  };
  const latency = requests
    ? `${number.format(periods.reduce((sum, p) => sum + p.avg_latency_ms * p.requests, 0) / requests)} ms avg`
    : "No requests";
  const models = [...breakdown.models].sort((a, b) => b.requests - a.requests);
  const topModels = models.slice(0, 4);
  const otherModels = models.slice(4).map((model) => model.name);
  const modelCount = (bucket: Bucket, name: string) =>
    Object.hasOwn(bucket.by_model, name) ? bucket.by_model[name] : 0;
  const modelSeries: Series[] = topModels.map((model, index) => ({
    name: model.name || "(not provided)",
    color: modelColors[index],
    points: periods.map((p) => ({
      label: p.label,
      start: p.start,
      value: modelCount(p, model.name),
    })),
  }));
  if (otherModels.length)
    modelSeries.push({
      name: "Other models",
      color: modelColors[4],
      points: periods.map((p) => ({
        label: p.label,
        start: p.start,
        value: otherModels.reduce((sum, name) => sum + modelCount(p, name), 0),
      })),
    });
  const providers = [...breakdown.providers].sort(
    (a, b) => b.requests - a.requests,
  );
  const providerLabel = (name: string) => name || "(not provided)";
  const topProviders = providers.slice(0, 4);
  const otherProviders = providers.slice(4);
  const providerGroups = [
    ...topProviders.map((provider, index) => ({
      name: providerLabel(provider.name),
      ids: [provider.name],
      color: modelColors[index],
    })),
    ...(otherProviders.length
      ? [
          {
            name: "Other providers",
            ids: otherProviders.map((provider) => provider.name),
            color: modelColors[4],
          },
        ]
      : []),
  ];
  const providerValue = (
    bucket: Bucket,
    ids: string[],
    tokens: boolean,
  ): number | null => {
    const entries = ids.flatMap((id) =>
      bucket.by_provider[id] ? [bucket.by_provider[id]] : [],
    );
    if (!tokens) return entries.reduce((sum, entry) => sum + entry.requests, 0);
    const values = entries
      .flatMap((entry) => [entry.input_tokens, entry.output_tokens])
      .filter((value): value is number => value !== null);
    return values.length ? values.reduce((sum, value) => sum + value, 0) : null;
  };
  const providerSeries = (tokens: boolean): Series[] =>
    providerGroups.map((group) => ({
      name: group.name,
      color: group.color,
      points: periods.flatMap((period) => {
        const value = providerValue(period, group.ids, tokens);
        return value === null
          ? []
          : [{ label: period.label, start: period.start, value }];
      }),
    }));
  const providerRows = (tokens: boolean) =>
    periodRows((period) =>
      providerGroups
        .map((group) => {
          const value = providerValue(period, group.ids, tokens);
          const partial =
            tokens &&
            group.ids.some((id) => {
              const entry = period.by_provider[id];
              return (
                entry &&
                (entry.input_tokens === null || entry.output_tokens === null)
              );
            });
          return `${group.name}: ${value === null ? "not reported" : `${number.format(value)}${partial ? " (partial)" : ""}`}`;
        })
        .join(", "),
    );
  const dateDescription = `Recorded requests grouped by UTC ${interval}.`;

  return (
    <div className="grid min-w-0 grid-cols-1 gap-3 @min-[768px]:grid-cols-2">
      <GraphPanel
        title="Request volume"
        timeSeries
        timeRange={timeRange}
        stacked
        summary={`${number.format(requests)} requests`}
        domain={domain}
        comparison={compare(requests, previousRequests, "requests")}
        description={`${dateDescription} Success: HTTP 200–399; error: HTTP 400 and above.`}
        unit="Requests"
        series={[
          {
            name: "Success",
            color: "#10b981",
            points: periods.map((p) => ({
              label: p.label,
              start: p.start,
              value: p.successes,
            })),
          },
          {
            name: "Error",
            color: "#f43f5e",
            points: periods.map((p) => ({
              label: p.label,
              start: p.start,
              value: p.errors,
            })),
          },
        ]}
        alternative={periodRows(volumeRows)}
      />
      <GraphPanel
        title="Average latency"
        timeSeries
        timeRange={timeRange}
        summary={latency}
        domain={domain}
        comparison={compare(
          averageLatency(periods),
          previousData ? averageLatency(previousData.buckets) : null,
          "ms average",
        )}
        description={`${dateDescription} Average request latency in milliseconds.`}
        unit="Milliseconds"
        series={[
          {
            name: "Latency",
            color: "#f59e0b",
            points: periods.map((p) => ({
              label: p.label,
              start: p.start,
              value: p.avg_latency_ms,
            })),
          },
        ]}
        alternative={periodRows((p) => `${p.avg_latency_ms} ms`)}
      />
      <GraphPanel
        title="Token usage"
        timeSeries
        timeRange={timeRange}
        stacked
        summary={`Input: ${tokenTotal("input_tokens")}; output: ${tokenTotal("output_tokens")} tokens`}
        domain={domain}
        comparisons={(
          [
            ["Input", "input_tokens"],
            ["Output", "output_tokens"],
          ] as const
        ).map(([name, field]) => ({
          title: `${name} tokens`,
          value: compare(
            totalTokens(periods, field),
            previousData ? totalTokens(previousData.buckets, field) : null,
            "tokens",
          ),
        }))}
        description={`${dateDescription} Input and output tokens are stacked. Unreported counts are omitted, not zero.`}
        unit="Tokens"
        emptyMessage={
          periods.length
            ? "Token counts were not reported."
            : "No requests to chart."
        }
        series={(
          [
            ["Input", "input_tokens", "#3b82f6"],
            ["Output", "output_tokens", "#a855f7"],
          ] as const
        ).map(([name, field, color]) => ({
          name,
          color,
          points: periods.flatMap((p) =>
            p[field] === null
              ? []
              : [{ label: p.label, start: p.start, value: p[field] }],
          ),
        }))}
        alternative={periodRows(
          (p) =>
            `Input: ${p.input_tokens === null ? "not reported" : `${number.format(p.input_tokens)} tokens`}; output: ${p.output_tokens === null ? "not reported" : `${number.format(p.output_tokens)} tokens`}`,
        )}
      />
      <GraphPanel
        title="Model usage"
        timeSeries
        timeRange={timeRange}
        stacked
        summary={`${number.format(requests)} requests`}
        domain={domain}
        description={`Recorded requests grouped by UTC ${interval}. Models beyond the four most used are combined as other models.`}
        unit="Requests"
        series={modelSeries}
        alternative={periods.map((p) => ({
          label: `${p.date} UTC`,
          value: models
            .map(
              (model) =>
                `${model.name || "(not provided)"}: ${number.format(modelCount(p, model.name))}`,
            )
            .join(", "),
        }))}
      />
      <GraphPanel
        title="Requests by provider"
        timeSeries
        timeRange={timeRange}
        stacked
        summary={`${number.format(requests)} requests`}
        domain={domain}
        comparison={requestComparison}
        description={`${dateDescription} Requests grouped by provider; providers beyond the four most used are combined.`}
        unit="Requests"
        series={providerSeries(false)}
        alternative={providerRows(false)}
      />
      <GraphPanel
        title="Token usage by provider"
        timeSeries
        timeRange={timeRange}
        stacked
        summary={`Input: ${tokenTotal("input_tokens")}; output: ${tokenTotal("output_tokens")} tokens`}
        domain={domain}
        description={`${dateDescription} Reported input and output tokens combined by provider. Missing counts are not zero.`}
        unit="Tokens"
        series={providerSeries(true)}
        alternative={providerRows(true)}
        emptyMessage={
          periods.length
            ? "Token counts were not reported."
            : "No requests to chart."
        }
      />
    </div>
  );
}
