import { createFileRoute } from "@tanstack/react-router";
import {
  ObservabilityFilters,
  updateObservabilityFilters,
  validateObservabilityFilters,
} from "@/components/ObservabilityFilters";
import { ObservabilitySkeleton } from "@/components/ObservabilitySkeleton";
import {
  ObservabilityError,
  RequestLogs,
  useObservability,
} from "@/components/Observability";
import { RefreshButton } from "@/components/RefreshButton";
import type { ObservabilityFilterValues } from "@/server/api/observability";

export const Route = createFileRoute("/llm-logs")({
  head: () => ({ meta: [{ title: "LLM Logs · Zero-G" }] }),
  validateSearch: validateObservabilityFilters,
  component: RoutedLlmLogsPage,
});

function RoutedLlmLogsPage() {
  const filters = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <LlmLogsPage
      filters={filters}
      onFiltersChange={(next) =>
        void navigate({
          search: (current) => updateObservabilityFilters(current, next),
        })
      }
    />
  );
}

export function LlmLogsPage({
  filters,
  onFiltersChange,
}: {
  filters: ObservabilityFilterValues;
  onFiltersChange: (filters: ObservabilityFilterValues) => void;
}) {
  const observability = useObservability(true, filters);
  const { data, loading, refetch, error } = observability;
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
          <h1 className="text-3xl font-semibold tracking-tight">LLM Logs</h1>
          <p className="mt-2 text-sm text-muted-foreground">
            Search and inspect recorded LLM requests.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <RefreshButton loading={loading} onClick={() => void refetch()} />
        </div>
      </header>
      <ObservabilityError error={error} refetch={refetch} />
      <div className="flex min-w-0 flex-col gap-4 md:flex-row md:items-start">
        <ObservabilityFilters
          filters={filters}
          options={data?.filter_options}
          onChange={(next) => {
            observability.setSelectedId(null);
            onFiltersChange(next);
          }}
        />
        <div className="min-w-0 flex-1">
          {loading && !data && <ObservabilitySkeleton logsOnly />}
          {data?.summary && <RequestLogs {...observability} />}
        </div>
      </div>
    </main>
  );
}
