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
import { Page } from "@/routes/__root";
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
    <Page
      busy={loading}
      title="LLM Logs"
      description="Search and inspect recorded LLM requests."
      actions={
        <RefreshButton loading={loading} onClick={() => void refetch()} />
      }
    >
      <ObservabilityError error={error} refetch={refetch} />
      <div className="flex min-w-0 flex-col gap-4 xl:flex-row xl:items-start">
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
    </Page>
  );
}
