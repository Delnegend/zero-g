import { useMemo } from "react";
import { barY, defineChart } from "@tanstack/charts";
import { Chart } from "@tanstack/charts/react";
import { scaleBand } from "@tanstack/charts/scales/band";
import { scaleLinear } from "@tanstack/charts/scales/linear";
import { tooltip } from "@tanstack/charts/tooltip";
import type { Log } from "./App";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "./components/ui/card";

export function ObservabilityCharts({ logs }: { logs: Log[] }) {
  const { days, invalidCount } = useMemo(() => {
    const counts = new Map<string, number>();
    let invalidCount = 0;
    for (const log of logs) {
      const date = new Date(log.created_at * 1000);
      if (Number.isNaN(date.getTime())) {
        invalidCount++;
        continue;
      }
      const day = date.toISOString().slice(0, 10);
      counts.set(day, (counts.get(day) ?? 0) + 1);
    }
    return {
      days: Array.from(counts, ([day, requests]) => ({ day, requests })).sort(
        (a, b) => a.day.localeCompare(b.day),
      ),
      invalidCount,
    };
  }, [logs]);

  const definition = useMemo(
    () =>
      defineChart({
        marks: [barY(days, { x: "day", y: "requests", fill: "currentColor" })],
        scales: {
          x: { scale: () => scaleBand().padding(0.18) },
          y: {
            scale: scaleLinear,
            nice: true,
            grid: true,
            axis: { label: "Requests" },
          },
        },
        tooltip,
      }),
    [days],
  );

  return (
    <Card className="mb-8">
      <CardHeader>
        <CardTitle>Requests by day</CardTitle>
        <CardDescription>
          Last 200 requests, grouped by UTC day. Only days with recorded
          requests are shown.
        </CardDescription>
      </CardHeader>
      <CardContent>
        {days.length === 0 ? (
          <p className="py-10 text-center text-sm text-muted-foreground">
            No requests to chart.
          </p>
        ) : (
          <>
            <div className="text-primary">
              <Chart
                definition={definition}
                height={280}
                ariaLabel="Daily request counts for the last 200 requests"
                ariaDescription="UTC days with recorded requests; exact counts follow the chart."
              />
            </div>
            <details className="mt-4 text-sm">
              <summary className="cursor-pointer text-muted-foreground">
                View daily counts
              </summary>
              <ul className="mt-2 space-y-1">
                {days.map(({ day, requests }) => (
                  <li key={day}>
                    {day} UTC: {requests.toLocaleString()}{" "}
                    {requests === 1 ? "request" : "requests"}
                  </li>
                ))}
              </ul>
            </details>
          </>
        )}
        {invalidCount > 0 && (
          <p className="mt-2 text-sm text-muted-foreground">
            {invalidCount}{" "}
            {invalidCount === 1 ? "request has" : "requests have"} an invalid
            timestamp and cannot be charted.
          </p>
        )}
      </CardContent>
    </Card>
  );
}
