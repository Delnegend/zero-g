import { useState } from "react";
import { useForm } from "@tanstack/react-form";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { PolarAngleAxis, RadialBar, RadialBarChart } from "recharts";
import { message } from "../server/api/client";
import {
  deleteConsoleSession,
  getCredentialUsage,
  saveConsoleSession,
  type QuotaWindow,
} from "../server/api/providers";
import { Alert, AlertDescription } from "./ui/alert";
import { Button } from "./ui/button";
import { ChartContainer } from "./ui/chart";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { Skeleton } from "./ui/skeleton";

const chartConfig = {
  remaining: { label: "Remaining", color: "var(--primary)" },
};

function WindowUsage({ window }: { window: QuotaWindow }) {
  const value = window.remaining_percent;
  const known =
    value !== null && Number.isFinite(value) && value >= 0 && value <= 100;
  const percent = known ? `${Number(value.toFixed(1))}%` : "Unknown";
  const reset =
    window.reset_at === null ? null : new Date(window.reset_at * 1000);
  const resetText =
    reset && Number.isFinite(reset.getTime()) ? reset.toLocaleString() : null;

  return (
    <div className="flex min-w-0 items-center gap-2">
      {known && (
        <div className="relative size-20 shrink-0" aria-hidden="true">
          <ChartContainer
            config={chartConfig}
            className="aspect-square size-full"
          >
            <RadialBarChart
              data={[{ remaining: value, fill: "var(--color-remaining)" }]}
              innerRadius="70%"
              outerRadius="100%"
              startAngle={90}
              endAngle={-270}
            >
              <PolarAngleAxis
                type="number"
                domain={[0, 100]}
                tick={false}
                tickLine={false}
                axisLine={false}
              />
              <RadialBar dataKey="remaining" background cornerRadius={5} />
            </RadialBarChart>
          </ChartContainer>
          <span className="absolute inset-0 flex items-center justify-center text-sm font-semibold">
            {percent}
          </span>
        </div>
      )}
      <div className="min-w-0 text-sm">
        <p className="break-words font-medium">{window.label}</p>
        <p className="text-muted-foreground">{percent} remaining</p>
        {resetText && (
          <p className="text-muted-foreground">Resets {resetText}</p>
        )}
      </div>
    </div>
  );
}

function ConsoleDialog({
  providerId,
  credentialId,
  credentialName,
  onClose,
}: {
  providerId: string;
  credentialId: string;
  credentialName: string;
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: ["credential-usage", providerId, credentialId],
    });
  const form = useForm({
    defaultValues: { workspace_id: "", session: "" },
    onSubmit: async ({ value }) => {
      setBusy(true);
      setError("");
      try {
        await saveConsoleSession(providerId, credentialId, value);
        await refresh();
        onClose();
      } catch {
        setError(
          "Could not save the console session. Check the workspace ID and try again.",
        );
      } finally {
        setBusy(false);
      }
    },
  });
  async function remove() {
    setBusy(true);
    setError("");
    try {
      await deleteConsoleSession(providerId, credentialId);
      await refresh();
      onClose();
    } catch {
      setError("Could not remove the console session. Try again.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent
        showCloseButton={!busy}
        className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
      >
        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            event.stopPropagation();
            void form.handleSubmit();
          }}
        >
          <DialogHeader>
            <DialogTitle>Zen console for {credentialName}</DialogTitle>
            <DialogDescription>
              Associate this credential with your Zen Console workspace to view
              its balance. Enter the workspace ID and the value of the
              __Host-console_session cookie, not an API key. The session is
              stored encrypted, expires, and may need to be replaced.
            </DialogDescription>
          </DialogHeader>
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          <form.Field name="workspace_id">
            {(field) => (
              <Label className="block text-sm font-medium">
                Workspace ID
                <Input
                  className="mt-1"
                  required
                  disabled={busy}
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
                />
              </Label>
            )}
          </form.Field>
          <form.Field name="session">
            {(field) => (
              <Label className="block text-sm font-medium">
                Console session
                <Input
                  className="mt-1"
                  type="password"
                  autoComplete="new-password"
                  required
                  disabled={busy}
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
                />
              </Label>
            )}
          </form.Field>
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={busy}>
              {busy ? "Working…" : "Save session"}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => void remove()}
            >
              Remove session
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={onClose}
            >
              Cancel
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

export function CredentialUsage({
  providerId,
  credentialId,
  credentialName,
  kind,
  connected,
}: {
  providerId: string;
  credentialId: string;
  credentialName: string;
  kind: string;
  connected: boolean;
}) {
  const [configuring, setConfiguring] = useState(false);
  const usage = useQuery({
    queryKey: ["credential-usage", providerId, credentialId],
    queryFn: ({ signal }) =>
      getCredentialUsage(providerId, credentialId, signal),
    enabled: connected,
    staleTime: 5 * 60 * 1000,
    refetchOnWindowFocus: false,
    retry: false,
  });

  return (
    <div
      className="min-w-0 space-y-2 sm:max-w-sm"
      aria-label={`Usage for ${credentialName}`}
    >
      {kind === "zen" && (
        <Button
          size="sm"
          variant="outline"
          onClick={() => setConfiguring(true)}
        >
          Configure console
        </Button>
      )}
      {connected && usage.isPending && (
        <Skeleton className="h-20 w-40 max-w-full" aria-label="Loading usage" />
      )}
      {connected && usage.isError && (
        <div className="space-y-2 text-sm">
          <p className="break-words text-destructive">
            Usage unavailable: {message(usage.error)}
          </p>
          <Button
            size="sm"
            variant="outline"
            onClick={() => void usage.refetch()}
          >
            Retry
          </Button>
        </div>
      )}
      {connected &&
        !usage.isError &&
        usage.data?.kind === "quota" &&
        (usage.data.windows.length ? (
          <div className="flex flex-wrap gap-3">
            {usage.data.windows.map((window, index) => (
              <WindowUsage key={`${window.label}-${index}`} window={window} />
            ))}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            No quota windows available.
          </p>
        ))}
      {connected && !usage.isError && usage.data?.kind === "balance" && (
        <p className="text-sm">
          <span className="text-muted-foreground">Balance: </span>
          <span className="font-semibold">
            {new Intl.NumberFormat(undefined, {
              style: "currency",
              currency: usage.data.currency,
            }).format(usage.data.amount)}
          </span>
        </p>
      )}
      {configuring && (
        <ConsoleDialog
          providerId={providerId}
          credentialId={credentialId}
          credentialName={credentialName}
          onClose={() => setConfiguring(false)}
        />
      )}
    </div>
  );
}
