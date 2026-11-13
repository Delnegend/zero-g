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
  useRequestLogs,
} from "@/components/Observability";
import { RefreshButton } from "@/components/RefreshButton";
import { Page } from "@/routes/__root";
import type {
  LogsQuery,
  LogsSort,
  ObservabilityFilterValues,
} from "@/server/api/observability";

type Paging = Pick<LogsQuery, "page" | "limit" | "search" | "sort">;
const validSort =
  /^(recent|(created_at|model|status|latency_ms|path):(asc|desc))$/;
function validateLogsSearch(
  search: Record<string, unknown>,
): ObservabilityFilterValues & Paging {
  const page = Number(search.page);
  const limit = Number(search.limit);
  return {
    ...validateObservabilityFilters(search),
    page: Number.isSafeInteger(page) && page > 0 ? page : 1,
    limit:
      Number.isSafeInteger(limit) && limit > 0 && limit <= 200 ? limit : 20,
    search: typeof search.search === "string" ? search.search : "",
    sort:
      typeof search.sort === "string" && validSort.test(search.sort)
        ? (search.sort as LogsSort)
        : "recent",
  };
}

export const Route = createFileRoute("/llm-logs")({
  head: () => ({ meta: [{ title: "LLM Logs · Zero-G" }] }),
  validateSearch: validateLogsSearch,
  component: RoutedLlmLogsPage,
});

function RoutedLlmLogsPage() {
  const filters = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <LlmLogsPage
      filters={filters}
      paging={filters}
      onPagingChange={(next) =>
        void navigate({ search: (current) => ({ ...current, ...next }) })
      }
      onFiltersChange={(next) =>
        void navigate({
          search: (current) => ({
            ...current,
            ...updateObservabilityFilters(current, next),
            page: 1,
          }),
        })
      }
    />
  );
}

export function LlmLogsPage({
  filters,
  onFiltersChange,
  paging,
  onPagingChange,
}: {
  filters: ObservabilityFilterValues;
  onFiltersChange: (filters: ObservabilityFilterValues) => void;
  paging: Paging;
  onPagingChange: (paging: Partial<Paging>) => void;
}) {
  const observability = useRequestLogs(filters, paging);
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
          {data && (
            <RequestLogs
              {...observability}
              data={
                observability.isPlaceholderData
                  ? { ...data, logs: [], total: 0 }
                  : data
              }
              paging={paging}
              onPagingChange={(next) => {
                observability.setSelectedId(null);
                onPagingChange(next);
              }}
            />
          )}
        </div>
      </div>
    </Page>
  );
}
