import { keepPreviousData, queryOptions } from "@tanstack/react-query";
import {
  getDashboard as requestDashboard,
  listLogs,
  getLog as requestLog,
} from "./generated/client";
import type {
  DashboardResponse,
  LogEntry,
  LogSummary,
  LogsResponse,
} from "./generated/schemas";

type Preset = "1h" | "6h" | "24h" | "7d" | "1m" | "6m" | "1y";
export type DashboardRange =
  | { kind: "preset"; value: Preset }
  | { kind: "custom"; from: number; until: number };

export type ObservabilityFilterValues = {
  providers?: string[];
  models?: string[];
  paths?: string[];
  statuses?: ("success" | "error")[];
  virtual_key_ids?: string[];
  credential_ids?: string[];
};

export type LogsSort =
  | "recent"
  | `${"created_at" | "model" | "status" | "latency_ms" | "path"}:${"asc" | "desc"}`;
export type LogsQuery = ObservabilityFilterValues & {
  page: number;
  limit: number;
  search: string;
  sort: LogsSort;
};

export function rangeStart(range: Preset, now = Date.now()): number {
  const date = new Date(now);
  if (range === "1h" || range === "6h" || range === "24h" || range === "7d") {
    const hours =
      range === "1h" ? 1 : range === "6h" ? 6 : range === "24h" ? 24 : 168;
    return Math.floor(date.getTime() / 1000) - hours * 3600;
  }
  const day = date.getUTCDate();
  date.setUTCDate(1);
  date.setUTCMonth(
    date.getUTCMonth() - (range === "1m" ? 1 : range === "6m" ? 6 : 12),
  );
  const lastDay = new Date(
    Date.UTC(date.getUTCFullYear(), date.getUTCMonth() + 1, 0),
  ).getUTCDate();
  date.setUTCDate(Math.min(day, lastDay));
  return Math.floor(date.getTime() / 1000);
}

export type ObservabilityComparison = {
  data: DashboardResponse | null;
  currentSince: number;
  currentUntil: number;
  previousSince: number;
  previousUntil: number;
  retentionCutoff: number | null;
};

export type Log = LogSummary;
export type Observability = DashboardResponse & {
  comparison?: ObservabilityComparison;
};
export type LogList = LogsResponse;

export function observabilityQueryOptions(
  range: DashboardRange,
  filters: ObservabilityFilterValues = {},
) {
  return queryOptions({
    queryKey: ["observability", { range, filters }],
    queryFn: async ({ signal }): Promise<Observability> => {
      const now = Math.floor(Date.now() / 1000);
      const currentUntil = range.kind === "preset" ? now : range.until;
      const currentSince =
        range.kind === "preset"
          ? rangeStart(range.value, currentUntil * 1000)
          : range.from;
      const previousUntil = currentSince - 1;
      const previousSince = previousUntil - (currentUntil - currentSince);
      const [current, previous] = await Promise.all([
        getDashboard(signal, {
          ...filters,
          since: currentSince,
          until: currentUntil,
        }),
        getDashboard(signal, {
          ...filters,
          since: previousSince,
          until: previousUntil,
        }).catch((error: unknown) => {
          if (signal.aborted) throw error;
          return null;
        }),
      ]);
      const retentionCutoff = Math.max(
        current.retention_cutoff,
        previous?.retention_cutoff ?? current.retention_cutoff,
      );
      return {
        ...current,
        comparison: {
          data: previousSince >= retentionCutoff ? previous : null,
          currentSince,
          currentUntil,
          previousSince,
          previousUntil,
          retentionCutoff,
        },
      };
    },
    placeholderData: keepPreviousData,
    retry: false,
    gcTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
}

export function logsQueryOptions(
  filters: ObservabilityFilterValues = {},
  paging: Pick<LogsQuery, "page" | "limit" | "search" | "sort"> = {
    page: 1,
    limit: 20,
    search: "",
    sort: "recent",
  },
) {
  return queryOptions({
    queryKey: ["observability-logs", { filters, paging }],
    queryFn: ({ signal }) => getLogs(signal, filters, paging),
    placeholderData: keepPreviousData,
    retry: false,
    gcTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
}

type QueryFilters = {
  since?: number;
  until?: number;
} & ObservabilityFilterValues;

function queryParams(range: QueryFilters = {}) {
  return {
    since: range.since,
    until: range.until,
    providers: range.providers?.length
      ? JSON.stringify(range.providers)
      : undefined,
    models: range.models?.length ? JSON.stringify(range.models) : undefined,
    paths: range.paths?.length ? JSON.stringify(range.paths) : undefined,
    statuses: range.statuses?.length
      ? JSON.stringify(range.statuses)
      : undefined,
    virtual_key_ids: range.virtual_key_ids?.length
      ? JSON.stringify(range.virtual_key_ids)
      : undefined,
    credential_ids: range.credential_ids?.length
      ? JSON.stringify(range.credential_ids)
      : undefined,
  };
}

function getDashboard(signal: AbortSignal, filters: QueryFilters) {
  return requestDashboard(queryParams(filters), {
    signal,
    statusOnlyError: true,
  });
}

function getLogs(
  signal: AbortSignal,
  filters: QueryFilters,
  paging: Pick<LogsQuery, "page" | "limit" | "search" | "sort">,
) {
  const { page, limit, search, sort } = paging;
  return listLogs(
    { ...queryParams(filters), page, limit, search, sort },
    { signal, statusOnlyError: true },
  );
}

export function getLog(signal: AbortSignal, id: Log["id"]): Promise<LogEntry> {
  return requestLog(id, { signal, statusOnlyError: true });
}
