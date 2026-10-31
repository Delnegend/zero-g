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
  data: ObservabilityResponse | null;
  currentSince: number;
  currentUntil: number;
  previousSince: number;
  previousUntil: number;
  retentionCutoff: number | null;
};

export type Observability = ObservabilityResponse & {
  comparison?: ObservabilityComparison;
};

export function observabilityQueryOptions(
  range: DashboardRange,
  logsOnly: boolean,
  filters: ObservabilityFilterValues = {},
) {
  return queryOptions({
    queryKey: ["observability", logsOnly ? { filters } : { range, filters }],
    queryFn: async ({ signal }): Promise<Observability> => {
      if (logsOnly)
        return getObservability(signal, { ...filters, logs_only: true });

      const now = Math.floor(Date.now() / 1000);
      const currentUntil = range.kind === "preset" ? now : range.until;
      const currentSince =
        range.kind === "preset"
          ? rangeStart(range.value, currentUntil * 1000)
          : range.from;
      const previousUntil = currentSince - 1;
      const previousSince = previousUntil - (currentUntil - currentSince);
      const [current, previous] = await Promise.all([
        getObservability(signal, {
          ...filters,
          since: currentSince,
          until: currentUntil,
        }),
        getObservability(signal, {
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

export type Log = LogEntry;

export function getObservability(
  signal: AbortSignal,
  range?: {
    since?: number;
    until?: number;
    logs_only?: boolean;
  } & ObservabilityFilterValues,
) {
  const params = range && {
    since: range.since,
    until: range.until,
    logs_only: range.logs_only,
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
