import { ResponseBody } from "@/components/ResponseBody";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Drawer,
  DrawerClose,
  DrawerContent,
  DrawerDescription,
  DrawerFooter,
  DrawerHeader,
  DrawerTitle,
} from "@/components/ui/drawer";
import { formatTimestamp } from "@/lib/date";
import { LogsTable } from "@/components/LogsTable";
import { RequestBody, requestBodyTheme } from "@/components/RequestBody";
import {
  getObservability,
  observabilityQueryOptions,
  type DashboardRange,
  type Log,
  type ObservabilityFilterValues,
} from "@/server/api/observability";
import { useQuery } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";

export const number = new Intl.NumberFormat();

export function formatTokens(value: number | null): string {
  return value == null ? "Unknown" : number.format(value);
}

export function useObservability(
  logsOnly = false,
  filters: ObservabilityFilterValues = {},
) {
  const [range, setRange] = useState<DashboardRange>({
    kind: "preset",
    value: "24h",
  });
  const {
    data,
    error: queryError,
    isFetching: loading,
    refetch,
  } = useQuery(observabilityQueryOptions(range, logsOnly, filters));
  const error =
    !loading && queryError
      ? queryError instanceof Error
        ? queryError.message
        : "Unable to load observability data."
      : null;
  const [selectedId, setSelectedId] = useState<Log["id"] | null>(null);
  const detailsTrigger = useRef<HTMLButtonElement>(null);
  const selectLog = useCallback((id: Log["id"], trigger: HTMLButtonElement) => {
    detailsTrigger.current = trigger;
    setSelectedId(id);
  }, []);

  const selected = data?.logs.find((log) => log.id === selectedId);
  const detail = useQuery({
    queryKey: ["observability-log", selectedId],
    queryFn: async ({ signal }) => {
      const result = await getObservability(signal, {
        logs_only: true,
        log_id: selectedId!,
      });
      const log = result.logs[0];
      if (!log || log.request_body == null || log.response_body == null) {
        throw new Error("Request details are no longer available.");
      }
      return log;
    },
    enabled: selectedId != null,
    retry: false,
    gcTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  return {
    range,
    setRange,
    data,
    loading,
    refetch,
    error,
    selected,
    detail,
    setSelectedId,
    detailsTrigger,
    selectLog,
  };
}

export function ObservabilityError({
  error,
  refetch,
}: {
  error: string | null;
  refetch: () => unknown;
}) {
  return (
    <>
      {error && (
        <Alert variant="destructive" className="mb-6">
          <AlertDescription>
            Unable to load observability data: {error}.{" "}
            <button
              type="button"
              className="font-medium underline underline-offset-4"
              onClick={() => void refetch()}
            >
              Try again
            </button>
          </AlertDescription>
        </Alert>
      )}
    </>
  );
}

export function RequestLogs({
  data,
  selected,
  detail,
  setSelectedId,
  detailsTrigger,
  selectLog,
}: ReturnType<typeof useObservability>) {
  return (
    <>
      <LogsTable logs={data?.logs ?? []} onSelect={selectLog} />
      <Drawer
        open={selected != null}
        onOpenChange={(open) => {
          if (!open) setSelectedId(null);
        }}
        swipeDirection="right"
        showSwipeHandle
      >
        <DrawerContent
          finalFocus={detailsTrigger}
          className="data-[swipe-axis=x]:[--drawer-content-width:min(100vw,48rem)] data-[swipe-axis=x]:sm:[--drawer-content-width:min(100vw,48rem)]"
        >
          {selected && (
            <>
              <DrawerHeader>
                <DrawerTitle>Request details</DrawerTitle>
                <DrawerDescription className="break-all">
                  {selected.provider} / {selected.model} ·{" "}
                  {formatTimestamp(selected.created_at)} · ID {selected.id}
                </DrawerDescription>
              </DrawerHeader>
              <div className="min-h-0 overflow-y-auto overscroll-contain p-4">
                <dl className="flex min-w-0 flex-wrap gap-x-8 gap-y-2 text-sm">
                  <div className="max-w-full min-w-0">
                    <dt className="text-muted-foreground">Path</dt>
                    <dd className="font-mono break-all">{selected.path}</dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">Status</dt>
                    <dd>{selected.status}</dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">Latency</dt>
                    <dd>{number.format(selected.latency_ms)} ms</dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">Tokens</dt>
                    <dd>
                      {formatTokens(selected.input_tokens)} in ·{" "}
                      {formatTokens(selected.output_tokens)} out
                    </dd>
                  </div>
                  <div className="max-w-full min-w-0">
                    <dt className="text-muted-foreground">Virtual key</dt>
                    <dd className="wrap-break-word">
                      {selected.virtual_key_name ??
                        selected.virtual_key_id ??
                        "Unknown"}
                    </dd>
                  </div>
                  <div className="max-w-full min-w-0">
                    <dt className="text-muted-foreground">
                      Provider credential
                    </dt>
                    <dd className="wrap-break-word">
                      {selected.credential_name ??
                        selected.credential_id ??
                        "Unknown"}
                    </dd>
                  </div>
                </dl>
                {detail.isPending ? (
                  <p className="mt-5" role="status">
                    Loading request details…
                  </p>
                ) : detail.error ? (
                  <Alert variant="destructive" className="mt-5">
                    <AlertDescription>
                      Unable to load request details.{" "}
                      <button
                        type="button"
                        className="underline"
                        onClick={() => void detail.refetch()}
                      >
                        Try again
                      </button>
                    </AlertDescription>
                  </Alert>
                ) : (
                  detail.data && (
                    <>
                      <style>{requestBodyTheme}</style>
                      <div className="mt-5 grid min-w-0 grid-cols-1 gap-4 lg:grid-cols-2">
                        <div className="min-w-0">
                          <h3 className="mb-2 text-sm font-medium">
                            Request body
                          </h3>
                          <RequestBody body={detail.data.request_body ?? ""} />
                        </div>
                        <ResponseBody
                          key={selected.id}
                          body={detail.data.response_body ?? ""}
                          status={selected.status}
                        />
                      </div>
                    </>
                  )
                )}
              </div>
              <DrawerFooter>
                <DrawerClose render={<Button variant="outline" />}>
                  Close
                </DrawerClose>
              </DrawerFooter>
            </>
          )}
        </DrawerContent>
      </Drawer>
    </>
  );
}
