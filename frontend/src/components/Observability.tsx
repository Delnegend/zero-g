import { useQuery } from "@tanstack/react-query";
import { useCallback, useRef, useState } from "react";
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
import { LogsTable } from "@/LogsTable";
import { RequestBody, requestBodyTheme } from "@/RequestBody";
import {
  observabilityQueryOptions,
  type DashboardRange,
  type Log,
} from "@/server/api/observability";

export const number = new Intl.NumberFormat();

export function formatTokens(value: number | null): string {
  return value == null ? "Unknown" : number.format(value);
}

function formatDate(value: number): string {
  const date = new Date(value * 1000);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

export function useObservability(logsOnly = false) {
  const [range, setRange] = useState<DashboardRange>({
    kind: "preset",
    value: "24h",
  });
  const {
    data,
    error: queryError,
    isFetching: loading,
    refetch,
  } = useQuery(observabilityQueryOptions(range, logsOnly));
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
  return {
    range,
    setRange,
    data,
    loading,
    refetch,
    error,
    selected,
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
                  {formatDate(selected.created_at)} · ID {selected.id}
                </DrawerDescription>
              </DrawerHeader>
              <div className="min-h-0 overflow-y-auto overscroll-contain p-4">
                <dl className="flex min-w-0 flex-wrap gap-x-8 gap-y-2 text-sm">
                  <div className="min-w-0 max-w-full">
                    <dt className="text-muted-foreground">Path</dt>
                    <dd className="break-all font-mono">{selected.path}</dd>
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
                </dl>
                <style>{requestBodyTheme}</style>
                <div className="mt-5 grid min-w-0 grid-cols-1 gap-4 lg:grid-cols-2">
                  {(
                    [
                      ["Request body", selected.request_body],
                      ["Response body", selected.response_body],
                    ] as const
                  ).map(([title, body]) => (
                    <div key={title} className="min-w-0">
                      <h3 className="mb-2 text-sm font-medium">{title}</h3>
                      <RequestBody body={body} />
                    </div>
                  ))}
                </div>
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
