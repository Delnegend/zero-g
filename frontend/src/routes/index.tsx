import { createFileRoute } from "@tanstack/react-router";
import { Activity, ArrowDownUp, Clock3 } from "lucide-react";
import { DateRangePicker } from "@/components/DateRangePicker";
import { ObservabilitySkeleton } from "@/components/Loading";
import { RefreshButton } from "@/components/RefreshButton";
import {
  formatTokens,
  number,
  ObservabilityError,
  RequestLogs,
  useObservability,
} from "@/components/Observability";
import { Card } from "@/components/ui/card";
import { ObservabilityCharts } from "@/ObservabilityCharts";

export const Route = createFileRoute("/")({
  head: () => ({ meta: [{ title: "Dashboard · Zero-G" }] }),
  component: ObservabilityPage,
});

export function ObservabilityPage() {
  const observability = useObservability();
  const { range, setRange, data, loading, refetch, error, setSelectedId } =
    observability;
  const summary = data?.summary;
  const cards = summary
    ? [
        {
          label: "Total requests",
          value: number.format(summary.requests),
          detail: "Recorded requests",
          icon: Activity,
        },
        {
          label: "Successful",
          value: number.format(summary.successes),
          detail: `${number.format(summary.errors)} errors`,
          icon: Activity,
        },
        {
          label: "Tokens",
          value:
            summary.input_tokens == null || summary.output_tokens == null
              ? "Unknown"
              : number.format(summary.input_tokens + summary.output_tokens),
          detail: `${formatTokens(summary.input_tokens)} in · ${formatTokens(summary.output_tokens)} out`,
          icon: ArrowDownUp,
        },
        {
          label: "Average latency",
          value: `${number.format(Math.round(summary.avg_latency_ms))} ms`,
          detail: "Across recorded requests",
          icon: Clock3,
        },
      ]
    : [];

  return (
    <main
      aria-busy={loading}
      className="mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12"
    >
      <header className="mb-8 flex flex-wrap items-start justify-between gap-4">
        <div>
          <p className="mb-2 text-xs font-semibold uppercase tracking-widest text-muted-foreground">
            Zero-G
          </p>
          <h1 className="text-3xl font-semibold tracking-tight">Dashboard</h1>
          <p className="mt-2 text-sm text-muted-foreground">
            Monitor requests, usage, and responses across your gateway.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <DateRangePicker
            value={range}
            onChange={(value) => {
              setSelectedId(null);
              setRange(value);
            }}
          />
          <RefreshButton loading={loading} onClick={() => void refetch()} />
        </div>
      </header>
      <ObservabilityError error={error} refetch={refetch} />
      {loading && !data && <ObservabilitySkeleton logsOnly={false} />}
      {summary && (
        <>
          <section
            aria-label="Request summary"
            className="mb-8 grid gap-4 sm:grid-cols-2 lg:grid-cols-4"
          >
            {cards.map(({ label, value, detail, icon: Icon }) => (
              <Card key={label} className="p-5">
                <div className="flex items-center justify-between gap-2 text-sm font-medium">
                  <h2>{label}</h2>
                  <Icon
                    className="size-4 text-muted-foreground"
                    aria-hidden="true"
                  />
                </div>
                <p className="mt-4 text-2xl font-semibold tracking-tight">
                  {value}
                </p>
                <p className="mt-1 text-xs text-muted-foreground">{detail}</p>
              </Card>
            ))}
          </section>
          {data && (
            <ObservabilityCharts
              interval={data.interval}
              buckets={data.buckets}
            />
          )}
          <RequestLogs {...observability} />
        </>
      )}
    </main>
  );
}
