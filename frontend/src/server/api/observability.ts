import { keepPreviousData, queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type { LogEntry, ObservabilityResponse } from "./generated/schemas";

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

export function rangeStart(range: Preset): number {
  const date = new Date();
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

export function observabilityQueryOptions(
  range: DashboardRange,
  logsOnly: boolean,
  filters: ObservabilityFilterValues = {},
) {
  return queryOptions({
    queryKey: ["observability", logsOnly ? { filters } : { range, filters }],
    queryFn: ({ signal }) => {
      if (logsOnly) return getObservability(signal, filters);
      if (range.kind === "preset") {
        const until = Math.floor(Date.now() / 1000);
        const since = rangeStart(range.value);
        return getObservability(signal, { since, until, ...filters });
      }
      return getObservability(signal, {
        since: range.from,
        until: range.until,
        ...filters,
      });
    },
    placeholderData: keepPreviousData,
    retry: false,
    gcTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
}

export type Log = LogEntry;
export type Observability = ObservabilityResponse;

export function getObservability(
  signal: AbortSignal,
  range?: { since?: number; until?: number } & ObservabilityFilterValues,
) {
  const params = range && {
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
  return api.getObservability(params, { signal, statusOnlyError: true });
}
