import { createFileRoute } from "@tanstack/react-router";
import { Minus, TrendingDown, TrendingUp } from "lucide-react";
import {
  ObservabilityFilters,
  updateObservabilityFilters,
  validateObservabilityFilters,
} from "@/components/ObservabilityFilters";
import { DateRangePicker } from "@/components/DateRangePicker";
import { ObservabilitySkeleton } from "@/components/ObservabilitySkeleton";
import { RefreshButton } from "@/components/RefreshButton";
import {
  formatTokens,
  number,
  ObservabilityError,
  useObservability,
} from "@/components/Observability";
import type { ObservabilityFilterValues } from "@/server/api/observability";
import { ObservabilityCharts } from "@/ObservabilityCharts";
import { Card } from "@/components/ui/card";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { ObservabilityComparison } from "@/server/api/observability";

function SummaryTrend({
  label,
  current,
  previous,
  comparison,
  lowerIsBetter = false,
}: {
  label: string;
  current: number | null;
  previous: number | null;
  comparison: ObservabilityComparison | undefined;
  lowerIsBetter?: boolean;
}) {
  if (!comparison?.data || current === null || previous === null) return null;

  const seconds = comparison.previousUntil - comparison.previousSince;
  const unit = seconds >= 86400 ? "day" : seconds >= 3600 ? "hour" : "minute";
  const duration =
    seconds / (unit === "day" ? 86400 : unit === "hour" ? 3600 : 60);
  const period = `previous ${new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 }).format(duration)} ${unit}${duration === 1 ? "" : "s"}`;
  const change =
    previous === 0
      ? current === 0
        ? 0
        : null
      : ((current - previous) / previous) * 100;
  const direction =
    change === null || change === 0
      ? "unchanged"
      : change > 0
        ? "increased"
        : "decreased";
  const Icon =
    change === null || change === 0
      ? Minus
      : change > 0
        ? TrendingUp
        : TrendingDown;
  const color =
    change === null || change === 0
      ? "text-muted-foreground"
      : change < 0 === lowerIsBetter
        ? "text-green-700 dark:text-green-400"
        : "text-red-700 dark:text-red-400";
  const value =
    change === null
      ? "New"
      : change !== 0 && Math.abs(change) < 0.01
        ? "<0.01%"
        : `${new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 }).format(Math.abs(change))}%`;
  const description =
    change === null
      ? `No percentage change is available because the ${period} was zero.`
      : `Compared with the ${period}.`;

  return (
    <Tooltip>
      <TooltipTrigger
        type="button"
        aria-label={`${label}: ${change === null ? "new" : `${direction} ${value}`}; ${description}`}
        className={`inline-flex shrink-0 items-center gap-1 text-xs font-medium ${color}`}
      >
        <Icon aria-hidden="true" className="size-4" />
        {value}
      </TooltipTrigger>
      <TooltipContent>{description}</TooltipContent>
    </Tooltip>
  );
}

export const Route = createFileRoute("/")({
  head: () => ({ meta: [{ title: "Dashboard · Zero-G" }] }),
  validateSearch: validateObservabilityFilters,
  component: RoutedObservabilityPage,
});

function RoutedObservabilityPage() {
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const filters: ObservabilityFilterValues = {
    providers: search.providers,
    models: search.models,
    paths: search.paths,
    statuses: search.statuses,
    virtual_key_ids: search.virtual_key_ids,
    credential_ids: search.credential_ids,
  };
  return (
    <ObservabilityPage
      filters={filters}
      onFiltersChange={(next) =>
        void navigate({
          search: (current) => updateObservabilityFilters(current, next),
        })
      }
    />
  );
}

export function ObservabilityPage({
  filters,
  onFiltersChange,
}: {
  filters: ObservabilityFilterValues;
  onFiltersChange: (filters: ObservabilityFilterValues) => void;
}) {
  const { range, setRange, data, loading, refetch, error } = useObservability(
    false,
    filters,
  );
  const summary = data?.summary;
  const previous = data?.comparison?.data?.summary;
  const cards = summary
    ? [
        {
          label: "Total requests",
          value: number.format(summary.requests),
          detail: "Recorded requests",
          current: summary.requests,
          previous: previous?.requests ?? null,
        },
        {
          label: "Successful",
          value: number.format(summary.successes),
          detail: `${number.format(summary.errors)} errors`,
          current: summary.successes,
          previous: previous?.successes ?? null,
        },
        {
          label: "Tokens",
          value:
            summary.input_tokens == null || summary.output_tokens == null
              ? "Unknown"
              : number.format(summary.input_tokens + summary.output_tokens),
          detail: `${formatTokens(summary.input_tokens)} in · ${formatTokens(summary.output_tokens)} out`,
          current:
            summary.input_tokens == null || summary.output_tokens == null
              ? null
              : summary.input_tokens + summary.output_tokens,
          previous:
            previous?.input_tokens == null || previous.output_tokens == null
              ? null
              : previous.input_tokens + previous.output_tokens,
        },
        {
          label: "Average latency",
          value: summary.requests
            ? `${number.format(Math.round(summary.avg_latency_ms))} ms`
            : "Unknown",
          detail: "Across recorded requests",
          current: summary.requests ? summary.avg_latency_ms : null,
          previous: previous?.requests ? previous.avg_latency_ms : null,
          lowerIsBetter: true,
        },
      ]
    : [];
  return (
    <main
      aria-busy={loading}
      className="mx-auto w-full min-w-0 max-w-7xl px-4 py-5 sm:px-6 lg:px-8"
    >
      <header className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold tracking-tight">Dashboard</h1>
        <div className="flex flex-wrap items-center gap-2">
          <DateRangePicker value={range} onChange={setRange} />
          <RefreshButton loading={loading} onClick={() => void refetch()} />
        </div>
      </header>
      <ObservabilityError error={error} refetch={refetch} />
      <div className="flex min-w-0 flex-col gap-4 md:flex-row md:items-start">
        <ObservabilityFilters
          filters={filters}
          options={data?.filter_options}
          onChange={onFiltersChange}
        />
        <div className="@container min-w-0 flex-1">
          {loading && !data && <ObservabilitySkeleton logsOnly={false} />}
          {data && (
            <>
              <section
                aria-label="Request summary"
                className="mb-3 grid min-w-0 grid-cols-2 gap-3 @min-[1008px]:grid-cols-4"
              >
                {cards.map(
                  ({
                    label,
                    value,
                    detail,
                    current,
                    previous,
                    lowerIsBetter,
                  }) => (
                    <Card key={label} className="min-w-0 gap-0 p-3 sm:p-5">
                      <div className="flex min-w-0 flex-wrap items-center justify-between gap-2">
                        <h2 className="text-sm font-medium">{label}</h2>
                        <SummaryTrend
                          label={label}
                          current={current}
                          previous={previous}
                          comparison={data.comparison}
                          lowerIsBetter={lowerIsBetter}
                        />
                      </div>
                      <p className="mt-4 text-2xl font-semibold tracking-tight">
                        {value}
                      </p>
                      <p className="mt-1 text-xs text-muted-foreground">
                        {detail}
                      </p>
                    </Card>
                  ),
                )}
              </section>
              <ObservabilityCharts
                interval={data.interval}
                buckets={data.buckets}
                breakdown={data.breakdown}
                comparison={data.comparison}
              />
            </>
          )}
        </div>
      </div>
    </main>
  );
}
