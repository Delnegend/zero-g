import { useEffect, useMemo, useState } from "react";
import { barY, defineChart, lineY } from "@tanstack/charts";
import { motion } from "@tanstack/charts/motion";
import { Chart } from "@tanstack/charts/react/core";
import { scaleBand } from "@tanstack/charts/scales/band";
import { scaleLinear } from "@tanstack/charts/scales/linear";
import { tooltip } from "@tanstack/charts/tooltip";
import { Minus, TrendingDown, TrendingUp } from "lucide-react";
import { Card } from "./components/ui/card";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "./components/ui/tooltip";
import type { ObservabilityComparison } from "./server/api/observability";

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
};
type Breakdown = { name: string; requests: number }[];
type Point = {
  label: string;
  value: number;
  previous?: boolean;
};
type Series = { name: string; color: string; points: Point[] };
type Row = { label: string; value: string };
type Comparison = {
  current: number | null;
  previous: number | null;
  detail: string;
};

function bucketIndex(start: number, interval: Interval) {
  const date = new Date(start * 1000);
  if (interval === "hour") return Math.floor(start / 3600);
  if (interval === "day") return Math.floor(start / 86400);
  if (interval === "month")
    return date.getUTCFullYear() * 12 + date.getUTCMonth();
  return date.getUTCFullYear();
}

function bucketStart(index: number, interval: Interval) {
  if (interval === "hour" || interval === "day")
    return index * (interval === "hour" ? 3600 : 86400);
  const date = new Date(0);
  const year = interval === "month" ? Math.floor(index / 12) : index;
  const month = interval === "month" ? ((index % 12) + 12) % 12 : 0;
  date.setUTCFullYear(year, month, 1);
  date.setUTCHours(0, 0, 0, 0);
  return date.getTime() / 1000;
}

function bucketPhase(start: number, interval: Interval) {
  const index = bucketIndex(start, interval);
  const boundary = bucketStart(index, interval);
  return (start - boundary) / (bucketStart(index + 1, interval) - boundary);
}

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
      ? "—"
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
const currentTooltip: typeof tooltip = {
  ...tooltip,
  create(context) {
    const instance = tooltip.create(context);
    return {
      ...instance,
      paint(paintContext) {
        if ((paintContext.point.datum as Point).previous) {
          instance.hide();
        } else {
          instance.paint(paintContext);
        }
      },
    };
  },
};
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
  previousSeries = [],
  previousRows = [],
  domain,
  comparison,
  summary,
  timeSeries = false,
  stacked = false,
  emptyMessage = "No requests to chart.",
}: {
  title: string;
  description: string;
  unit: string;
  series: Series[];
  alternative: Row[];
  previousSeries?: Series[];
  previousRows?: Row[];
  domain: string[];
  comparison?: Comparison;
  summary: string;
  timeSeries?: boolean;
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
  const pointCount = [...series, ...previousSeries].reduce(
    (total, item) => total + item.points.length,
    0,
  );
  const definition = useMemo(() => {
    // Give each grouped series its own band so its previous line and bar share
    // the same horizontal position.
    const grouped = !stacked && series.length > 1;
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
    const lines = (items: Series[]) => {
      return items.flatMap((item) => {
        const points = new Map(
          item.points.map((point) => [point.label, point]),
        );
        const segments: Point[][] = [];
        let segment: Point[] = [];
        for (const label of domain) {
          const point = points.get(label);
          if (point) {
            segment.push({ ...point, previous: true });
          } else if (segment.length) {
            segments.push(segment);
            segment = [];
          }
        }
        if (segment.length) segments.push(segment);
        return segments.map((part) =>
          lineY(part, {
            x: (point) => slot(point.label, item.name),
            y: "value",
            stroke: item.color,
            points: true,
            strokeDasharray: "5 4",
          }),
        );
      });
    };
    return defineChart({
      marks: [
        barY(bars, {
          x: (point) => slot(point.label, point.series),
          y: "value",
          z: "series",
          ...(stacked ? { layout: { type: "stack" as const } } : {}),
          fill: (point) => point.color,
          fillOpacity: 0.55,
          maxThickness: 30,
        }),
        ...lines(previousSeries),
      ],
      scales: {
        x: {
          scale: scaleBand().domain(chartDomain).padding(0.18),
          axis: {
            ticks: {
              spacing: 70,
              format: (value) => {
                const label = labels.get(String(value)) ?? String(value);
                return timeSeries
                  ? label.includes(" ")
                    ? label.slice(5)
                    : label
                  : label.length > 12
                    ? `${label.slice(0, 11)}…`
                    : label;
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
        use: currentTooltip,
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
          {
            channel: "y",
            label: unit,
          },
        ],
      },
    });
  }, [series, previousSeries, unit, domain, timeSeries, stacked]);

  return (
    <Card className="flex min-w-0 flex-col gap-0 p-3 shadow-none sm:p-5">
      <div className="flex min-h-8 min-w-0 items-start justify-between gap-2">
        <div className="flex min-w-0 flex-wrap items-baseline gap-x-2">
          <h2 className="text-sm font-medium">{title}</h2>
          <span className="text-xs text-muted-foreground">{summary}</span>
        </div>
        {comparison && <GraphTrend title={title} comparison={comparison} />}
      </div>
      <div className="request-chart h-[135px] min-w-0 shrink-0 overflow-hidden text-foreground sm:h-[205px]">
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
            ariaDescription={`${description}${previousSeries.some((item) => item.points.length > 0) ? " Previous values are aligned by offsets from the selected period boundaries." : ""} Exact values follow the chart.`}
          />
        )}
      </div>
      {(series.length > 0 || previousSeries.length > 0) && (
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
          {previousSeries.some((item) => item.points.length > 0) && (
            <span className="inline-flex items-center gap-1">
              <span
                aria-hidden="true"
                className="w-4 border-t-2 border-dashed border-current"
              />
              Previous period
            </span>
          )}
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
      {previousRows.length > 0 && (
        <ul className="sr-only" aria-label={`${title} previous period data`}>
          {previousRows.map((row, index) => (
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
  const firstStart = buckets.reduce(
    (first, bucket) => Math.min(first, bucket.start),
    Infinity,
  );
  const currentBase = bucketIndex(
    comparison?.currentSince ?? (Number.isFinite(firstStart) ? firstStart : 0),
    interval,
  );
  const positionLabel = (position: number) => {
    const index = currentBase + position;
    const seconds = bucketStart(index, interval);
    const outside =
      comparison &&
      (index < currentBase ||
        index > bucketIndex(comparison.currentUntil, interval));
    return `${bucketLabel(seconds, interval)}${outside ? "*" : ""}`;
  };
  const previousData = comparison?.data ?? null;
  const previousSince = comparison?.previousSince ?? 0;
  const phaseDifference = comparison
    ? bucketPhase(previousSince, interval) -
      bucketPhase(comparison.currentSince, interval)
    : 0;
  // Align the closest bucket boundaries, including tiny partial edge buckets.
  const previousBase =
    bucketIndex(previousSince, interval) +
    (phaseDifference > 0.5 ? 1 : phaseDifference < -0.5 ? -1 : 0);
  const makePeriods = (items: Bucket[], base: number) =>
    [...items]
      .sort((a, b) => a.start - b.start)
      .map((bucket) => ({
        ...bucket,
        position: bucketIndex(bucket.start, interval) - base,
        label: positionLabel(bucketIndex(bucket.start, interval) - base),
        date: bucketLabel(bucket.start, interval),
      }));
  const periods = makePeriods(buckets, currentBase);
  const previous =
    previousData && previousData.interval === interval
      ? makePeriods(previousData.buckets, previousBase)
      : [];
  const positions = [
    ...new Set([...periods, ...previous].map((p) => p.position)),
  ].sort((a, b) => a - b);
  const domain = positions.length
    ? Array.from(
        { length: positions[positions.length - 1] - positions[0] + 1 },
        (_, index) => positionLabel(positions[0] + index),
      )
    : [];
  const rows = (
    items: typeof periods,
    value: (bucket: (typeof periods)[number]) => string,
  ) =>
    items.map((bucket) => ({
      label: `${bucket.date} UTC (aligned with ${bucket.label} UTC)`,
      value: value(bucket),
    }));
  const periodRows = (value: (bucket: (typeof periods)[number]) => string) =>
    rows(periods, value);
  const previousRows = (value: (bucket: (typeof periods)[number]) => string) =>
    rows(previous, value);
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
    timeSeries = false,
  ): Comparison => ({
    current,
    previous: unavailable ? null : previous,
    detail: `${unavailable ?? (previous === null ? `${period}: not reported.` : `${period}: ${number.format(previous)} ${unit}.${current === null ? " The current value was not reported." : ""}`)}${
      previousData ? " Older data may be missing." : ""
    }${
      timeSeries && previousData && previousData.interval !== interval
        ? " The dashed line is unavailable because the periods use different time intervals."
        : timeSeries &&
            previousData &&
            previous !== null &&
            previousData.buckets.length > 0 &&
            periods.length > 0
          ? ` The dashed line aligns bucket offsets from the start of each selected period.${domain.some((label) => label.endsWith("*")) ? " Dates marked * are outside the selected range." : ""}`
          : ""
    }`,
  });
  const requestComparison = compare(
    periods.reduce((sum, p) => sum + p.requests, 0),
    previousRequests,
    "requests",
  );
  const volumeRows = (p: (typeof periods)[number]) =>
    `${number.format(p.requests)} requests; ${number.format(p.successes)} successful, ${number.format(p.errors)} errors`;
  const historical = (
    name: string,
    color: string,
    value: (p: (typeof periods)[number]) => number | null,
  ): Series => ({
    name,
    color,
    points: previous.flatMap((p) => {
      const result = value(p);
      return result === null ? [] : [{ label: p.label, value: result }];
    }),
  });
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
      value: modelCount(p, model.name),
    })),
  }));
  if (otherModels.length)
    modelSeries.push({
      name: "Other models",
      color: modelColors[4],
      points: periods.map((p) => ({
        label: p.label,
        value: otherModels.reduce((sum, name) => sum + modelCount(p, name), 0),
      })),
    });
  const providers = [
    ...new Set(
      [
        ...breakdown.providers,
        ...(previousData?.breakdown.providers ?? []),
      ].map((item) => item.name),
    ),
  ];
  const providerLabel = (name: string) => name || "(not provided)";
  const providerCount = (items: Breakdown, name: string) =>
    items.find((item) => item.name === name)?.requests ?? 0;
  const dateDescription = `Recorded requests grouped by UTC ${interval}. Buckets are aligned by their offsets from the selected period boundaries; gaps remain in their original positions. Edge buckets can cover different durations.`;

  return (
    <div className="grid min-w-0 grid-cols-1 gap-3 @min-[672px]:grid-cols-2">
      <GraphPanel
        title="Request volume"
        timeSeries
        stacked
        summary={`${number.format(requests)} requests`}
        domain={domain}
        comparison={compare(requests, previousRequests, "requests", true)}
        previousSeries={[
          historical("Success", "#10b981", (p) => p.successes),
          historical("Error", "#f43f5e", (p) => p.errors),
        ]}
        previousRows={previousRows(volumeRows)}
        description={`${dateDescription} Success: HTTP 200–399; error: HTTP 400 and above.`}
        unit="Requests"
        series={[
          {
            name: "Success",
            color: "#10b981",
            points: periods.map((p) => ({
              label: p.label,
              value: p.successes,
            })),
          },
          {
            name: "Error",
            color: "#f43f5e",
            points: periods.map((p) => ({ label: p.label, value: p.errors })),
          },
        ]}
        alternative={periodRows(volumeRows)}
      />
      <GraphPanel
        title="Average latency"
        timeSeries
        summary={latency}
        domain={domain}
        comparison={compare(
          averageLatency(periods),
          previousData ? averageLatency(previousData.buckets) : null,
          "ms average",
          true,
        )}
        previousSeries={[
          historical("Latency", "#f59e0b", (p) => p.avg_latency_ms),
        ]}
        previousRows={previousRows((p) => `${p.avg_latency_ms} ms`)}
        description={`${dateDescription} Average request latency in milliseconds.`}
        unit="Milliseconds"
        series={[
          {
            name: "Latency",
            color: "#f59e0b",
            points: periods.map((p) => ({
              label: p.label,
              value: p.avg_latency_ms,
            })),
          },
        ]}
        alternative={periodRows((p) => `${p.avg_latency_ms} ms`)}
      />
      {(
        [
          ["Input", "input_tokens", "#3b82f6"],
          ["Output", "output_tokens", "#a855f7"],
        ] as const
      ).map(([name, field, color]) => (
        <GraphPanel
          key={field}
          title={`${name} tokens`}
          timeSeries
          summary={`${tokenTotal(field)} tokens`}
          domain={domain}
          comparison={compare(
            totalTokens(periods, field),
            previousData ? totalTokens(previousData.buckets, field) : null,
            "tokens",
            true,
          )}
          previousSeries={[historical(name, color, (p) => p[field])]}
          previousRows={previousRows((p) =>
            p[field] === null
              ? "not reported"
              : `${number.format(p[field])} tokens`,
          )}
          description={`${dateDescription} Unreported token counts are omitted, not zero.`}
          unit="Tokens"
          emptyMessage={
            periods.length
              ? `${name} tokens were not reported.`
              : "No requests to chart."
          }
          series={[
            {
              name,
              color,
              points: periods.flatMap((p) =>
                p[field] === null ? [] : [{ label: p.label, value: p[field] }],
              ),
            },
          ]}
          alternative={periodRows((p) =>
            p[field] === null
              ? "not reported"
              : `${number.format(p[field])} tokens`,
          )}
        />
      ))}
      <GraphPanel
        title="Model usage"
        timeSeries
        stacked
        summary={`${number.format(requests)} requests`}
        domain={
          periods.length
            ? Array.from(
                {
                  length:
                    periods[periods.length - 1].position -
                    periods[0].position +
                    1,
                },
                (_, index) => positionLabel(periods[0].position + index),
              )
            : []
        }
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
        summary={`${number.format(requests)} requests`}
        domain={providers.map(providerLabel)}
        comparison={requestComparison}
        previousSeries={
          previousData
            ? [
                {
                  name: "Requests",
                  color: "var(--primary)",
                  points: providers.map((name) => ({
                    label: providerLabel(name),
                    value: providerCount(
                      previousData.breakdown.providers,
                      name,
                    ),
                  })),
                },
              ]
            : []
        }
        previousRows={
          previousData
            ? providers.map((name) => ({
                label: providerLabel(name),
                value: `${number.format(providerCount(previousData.breakdown.providers, name))} requests`,
              }))
            : []
        }
        description="Request distribution across providers in the selected range."
        unit="Requests"
        series={[
          {
            name: "Requests",
            color: "var(--primary)",
            points: providers.map((name) => ({
              label: providerLabel(name),
              value: providerCount(breakdown.providers, name),
            })),
          },
        ]}
        alternative={breakdown.providers.map((item) => ({
          label: item.name || "(not provided)",
          value: `${number.format(item.requests)} requests`,
        }))}
      />
    </div>
  );
}
