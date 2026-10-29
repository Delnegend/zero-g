import { useMemo } from "react";
import { barY, defineChart } from "@tanstack/charts";
import { motion } from "@tanstack/charts/motion";
import { Chart } from "@tanstack/charts/react/core";
import { scaleBand } from "@tanstack/charts/scales/band";
import { scaleLinear } from "@tanstack/charts/scales/linear";
import { tooltip } from "@tanstack/charts/tooltip";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "./components/ui/card";

const chartRenderer = motion({
  transition: { type: "spring", stiffness: 170, damping: 18, mass: 1 },
});

export function ObservabilityCharts({
  interval,
  buckets,
}: {
  interval: "hour" | "day" | "month" | "year";
  buckets: { start: number; requests: number }[];
}) {
  const periods = useMemo(
    () =>
      [...buckets]
        .sort((a, b) => a.start - b.start)
        .map(({ start, requests }) => {
          const iso = new Date(start * 1000).toISOString();
          const label =
            interval === "hour"
              ? `${iso.slice(0, 10)} ${iso.slice(11, 16)}`
              : iso.slice(
                  0,
                  interval === "day" ? 10 : interval === "month" ? 7 : 4,
                );
          return { label, requests };
        }),
    [buckets, interval],
  );

  const definition = useMemo(
    () =>
      defineChart({
        marks: [
          barY(periods, {
            x: "label",
            y: "requests",
            fill: "url(#requests-fill)",
            radius: 4,
            maxThickness: 48,
          }),
        ],
        scales: {
          x: { scale: () => scaleBand().padding(0.18) },
          y: {
            scale: scaleLinear,
            nice: true,
            grid: true,
            axis: { label: "Requests" },
          },
        },
        gradients: [
          {
            id: "requests-fill",
            x1: 0,
            y1: 1,
            x2: 0,
            y2: 0,
            stops: [
              { offset: 0, color: "var(--primary)", opacity: 0.45 },
              { offset: 1, color: "var(--primary)", opacity: 1 },
            ],
          },
        ],
        tooltip: {
          use: tooltip,
          items: [
            { channel: "x", label: `UTC ${interval}` },
            { channel: "y", label: "Requests" },
          ],
        },
      }),
    [periods, interval],
  );

  return (
    <Card className="mb-8">
      <CardHeader>
        <CardTitle>Request by {interval}</CardTitle>
        <CardDescription>
          All requests in the selected range, grouped by UTC {interval}. Only
          periods with recorded requests are shown.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {periods.length === 0 ? (
          <p className="py-10 text-center text-sm text-muted-foreground">
            No requests to chart.
          </p>
        ) : (
          <>
            <div className="request-chart text-foreground">
              <Chart
                definition={definition}
                renderer={chartRenderer}
                height={280}
                ariaLabel={`Request counts by UTC ${interval} for the selected range`}
                ariaDescription={`UTC ${interval} periods with recorded requests; exact counts follow the chart.`}
              />
            </div>
            <details className="mt-4 text-sm">
              <summary className="cursor-pointer text-muted-foreground">
                View counts by {interval}
              </summary>
              <ul className="mt-2 space-y-1">
                {periods.map(({ label, requests }) => (
                  <li key={label}>
                    {label} UTC: {requests.toLocaleString()}{" "}
                    {requests === 1 ? "request" : "requests"}
                  </li>
                ))}
              </ul>
            </details>
          </>
        )}
      </CardContent>
    </Card>
  );
}
