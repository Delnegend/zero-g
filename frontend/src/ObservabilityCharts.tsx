import { useEffect, useMemo, useState } from "react";
import { barY, defineChart, lineY } from "@tanstack/charts";
import { motion } from "@tanstack/charts/motion";
import { Chart } from "@tanstack/charts/react/core";
import { scaleBand } from "@tanstack/charts/scales/band";
import { scaleLinear } from "@tanstack/charts/scales/linear";
import { tooltip } from "@tanstack/charts/tooltip";
import { BarChart3, LineChart } from "lucide-react";
import { Button } from "./components/ui/button";
import { Card } from "./components/ui/card";

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
type Point = { label: string; value: number };
type Series = { name: string; color: string; points: Point[] };
type Row = { label: string; value: string };

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
  summary,
  timeSeries = false,
  emptyMessage = "No requests to chart.",
}: {
  title: string;
  description: string;
  unit: string;
  series: Series[];
  alternative: Row[];
  summary: string;
  timeSeries?: boolean;
  emptyMessage?: string;
}) {
  const [chartType, setChartType] = useState<"bar" | "line">("bar");
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
    const bars = series.flatMap((item) =>
      item.points.map((point) => ({
        ...point,
        series: item.name,
        color: item.color,
      })),
    );
    return defineChart({
      marks:
        chartType === "bar"
          ? [
              barY(bars, {
                x: "label",
                y: "value",
                z: "series",
                fill: (point) => point.color,
                layout: { type: "stack" },
                maxThickness: 30,
              }),
            ]
          : series.flatMap((item) => {
              // Missing token observations break lines instead of becoming zero.
              const points = new Map(
                item.points.map((point) => [point.label, point]),
              );
              const segments: Point[][] = [];
              let segment: Point[] = [];
              for (const row of alternative) {
                const point = points.get(row.label.replace(/ UTC$/, ""));
                if (point) segment.push(point);
                else if (segment.length) {
                  segments.push(segment);
                  segment = [];
                }
              }
              if (segment.length) segments.push(segment);
              return segments.map((part) =>
                lineY(part, {
                  x: "label",
                  y: "value",
                  stroke: item.color,
                  points: true,
                }),
              );
            }),
      scales: {
        x: {
          scale: () =>
            scaleBand()
              .domain(alternative.map((row) => row.label.replace(/ UTC$/, "")))
              .padding(0.18),
          axis: {
            ticks: {
              spacing: 70,
              format: (value) => {
                const label = String(value);
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
        use: tooltip,
        items: [
          { channel: "x", label: timeSeries ? "Period (UTC)" : "Category" },
          { channel: "y", label: unit },
        ],
      },
    });
  }, [series, unit, chartType, alternative, timeSeries]);

  return (
    <Card className="flex h-[260px] min-w-0 flex-col gap-0 overflow-hidden rounded-sm p-2 shadow-none sm:h-[330px]">
      <div className="shrink-0 px-1 text-sm font-medium">{title}</div>
      <div className="flex min-h-8 min-w-0 items-center justify-between gap-2 px-1">
        <p className="min-w-0 text-xs text-muted-foreground">
          <span className="font-semibold text-foreground">{summary}</span>
        </p>
        {timeSeries && (
          <div
            className="flex shrink-0"
            role="group"
            aria-label={`${title} chart type`}
          >
            <Button
              type="button"
              size="icon-sm"
              variant={chartType === "bar" ? "secondary" : "ghost"}
              aria-label={`Show ${title} as bars`}
              aria-pressed={chartType === "bar"}
              onClick={() => setChartType("bar")}
            >
              <BarChart3 aria-hidden="true" />
            </Button>
            <Button
              type="button"
              size="icon-sm"
              variant={chartType === "line" ? "secondary" : "ghost"}
              aria-label={`Show ${title} as a line`}
              aria-pressed={chartType === "line"}
              onClick={() => setChartType("line")}
            >
              <LineChart aria-hidden="true" />
            </Button>
          </div>
        )}
      </div>
      {series.length > 1 && (
        <div className="flex min-w-0 shrink-0 flex-wrap gap-x-3 gap-y-0.5 px-1 text-xs text-muted-foreground">
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
      <div className="request-chart min-h-0 min-w-0 flex-1 overflow-hidden text-foreground">
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
}: {
  interval: Interval;
  buckets: Bucket[];
  breakdown: { providers: Breakdown; models: Breakdown };
}) {
  const periods = useMemo(
    () =>
      [...buckets]
        .sort((a, b) => a.start - b.start)
        .map((bucket) => ({
          ...bucket,
          label: bucketLabel(bucket.start, interval),
        })),
    [buckets, interval],
  );
  const periodRows = (value: (bucket: (typeof periods)[number]) => string) =>
    periods.map((bucket) => ({
      label: `${bucket.label} UTC`,
      value: value(bucket),
    }));
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
  const otherModels = models.slice(4);
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
        value: otherModels.reduce(
          (sum, model) => sum + modelCount(p, model.name),
          0,
        ),
      })),
    });
  const dateDescription = `Recorded requests grouped by UTC ${interval}. Only periods with requests are shown.`;

  return (
    <div className="grid min-w-0 grid-cols-1 gap-2 @min-[672px]:grid-cols-2 @min-[1008px]:grid-cols-3">
      <GraphPanel
        title="Request volume"
        timeSeries
        summary={`${number.format(requests)} requests`}
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
        alternative={periodRows(
          (p) =>
            `${number.format(p.requests)} requests; ${number.format(p.successes)} successful, ${number.format(p.errors)} errors`,
        )}
      />
      <GraphPanel
        title="Token usage"
        timeSeries
        summary={`${tokenTotal("input_tokens")} input · ${tokenTotal("output_tokens")} output reported`}
        description={`${dateDescription} Unreported token counts are omitted, not zero.`}
        unit="Tokens"
        emptyMessage={
          periods.length
            ? "Token usage was not reported."
            : "No requests to chart."
        }
        series={[
          {
            name: "Input",
            color: "#3b82f6",
            points: periods.flatMap((p) =>
              p.input_tokens === null
                ? []
                : [{ label: p.label, value: p.input_tokens }],
            ),
          },
          {
            name: "Output",
            color: "#a855f7",
            points: periods.flatMap((p) =>
              p.output_tokens === null
                ? []
                : [{ label: p.label, value: p.output_tokens }],
            ),
          },
        ]}
        alternative={periodRows(
          (p) =>
            `input: ${p.input_tokens === null ? "not reported" : number.format(p.input_tokens)}; output: ${p.output_tokens === null ? "not reported" : number.format(p.output_tokens)}`,
        )}
      />
      <GraphPanel
        title="Average latency"
        timeSeries
        summary={latency}
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
        alternative={periodRows((p) => `${number.format(p.avg_latency_ms)} ms`)}
      />
      <GraphPanel
        title="Model usage"
        timeSeries
        summary={`${number.format(requests)} requests`}
        description={`${dateDescription} Models beyond the four most used are combined as other models.`}
        unit="Requests"
        series={modelSeries}
        alternative={periodRows((p) =>
          models
            .map(
              (model) =>
                `${model.name || "(not provided)"}: ${number.format(modelCount(p, model.name))}`,
            )
            .join(", "),
        )}
      />
      <GraphPanel
        title="Requests by provider"
        summary={`${number.format(requests)} requests`}
        description="Request distribution across providers in the selected range."
        unit="Requests"
        series={[
          {
            name: "Requests",
            color: "var(--primary)",
            points: breakdown.providers.map((item) => ({
              label: item.name || "(not provided)",
              value: item.requests,
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
