import { keepPreviousData, queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type { LogEntry, ObservabilityResponse } from "./generated/schemas";

type Preset = "24h" | "7d" | "1m" | "6m" | "1y";
export type DashboardRange =
  | { kind: "preset"; value: Preset }
  | { kind: "custom"; from: number; until: number };

export function rangeStart(range: Preset): number {
  const date = new Date();
  if (range === "24h" || range === "7d") {
    return Math.floor(
      (date.getTime() - (range === "24h" ? 24 : 7 * 24) * 3600_000) / 1000,
    );
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
) {
  return queryOptions({
    queryKey: ["observability", logsOnly ? "all" : range],
    queryFn: ({ signal }) => {
      if (logsOnly) return getObservability(signal);
      if (range.kind === "preset") {
        const until = Math.floor(Date.now() / 1000);
        const since =
          range.value === "24h"
            ? until - 24 * 60 * 60
            : rangeStart(range.value);
        return getObservability(signal, { since, until });
      }
      return getObservability(signal, {
        since: range.from,
        until: range.until,
      });
    },
    placeholderData: logsOnly ? undefined : keepPreviousData,
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
  range?: { since: number; until: number },
) {
  return api.getObservability(range, { signal, statusOnlyError: true });
}
