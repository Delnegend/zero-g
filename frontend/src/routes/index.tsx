import { createFileRoute } from "@tanstack/react-router";
import {
  ObservabilityFilters,
  updateObservabilityFilters,
  validateObservabilityFilters,
} from "@/components/ObservabilityFilters";
import { DateRangePicker } from "@/components/DateRangePicker";
import { ObservabilitySkeleton } from "@/components/ObservabilitySkeleton";
import { RefreshButton } from "@/components/RefreshButton";
import {
  ObservabilityError,
  useObservability,
} from "@/components/Observability";
import type { ObservabilityFilterValues } from "@/server/api/observability";
import { ObservabilityCharts } from "@/ObservabilityCharts";

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
            <ObservabilityCharts
              interval={data.interval}
              buckets={data.buckets}
              breakdown={data.breakdown}
            />
          )}
        </div>
      </div>
    </main>
  );
}
