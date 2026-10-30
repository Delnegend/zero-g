import { useEffect, useState } from "react";
import { useForm } from "@tanstack/react-form";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { message } from "../server/api/client";
import {
  deleteConsoleSession,
  deleteOpenrouterManagementKey,
  credentialUsageOptions,
  saveConsoleSession,
  saveOpenrouterManagementKey,
  type QuotaWindow,
} from "../server/api/providers";
import { Alert, AlertDescription } from "./ui/alert";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { Progress } from "./ui/progress";
import { Skeleton } from "./ui/skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";

function ResetTime({ reset }: { reset: Date }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(timer);
  }, []);
  const minutes = Math.ceil((reset.getTime() - now) / 60_000);
  const days = Math.floor(minutes / 1_440);
  const hours = Math.floor((minutes % 1_440) / 60);
  const remainingMinutes = minutes % 60;
  const relative = [
    days > 0 && `${days}d`,
    hours > 0 && `${hours}h`,
    remainingMinutes > 0 && `${remainingMinutes}m`,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <Tooltip>
      <TooltipTrigger className="text-left text-muted-foreground underline decoration-dotted underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2">
        {minutes > 0 ? `Resets in ${relative}` : "Reset due"}
      </TooltipTrigger>
      <TooltipContent>{reset.toLocaleString()}</TooltipContent>
    </Tooltip>
  );
}

function WindowUsage({ window }: { window: QuotaWindow }) {
  const value = window.remaining_percent;
  const known =
    value !== null && Number.isFinite(value) && value >= 0 && value <= 100;
  const percent = known ? `${Number(value.toFixed(1))}%` : "Unknown";
  const reset =
    window.reset_at === null ? null : new Date(window.reset_at * 1000);

  return (
    <div className="@container min-w-0 text-sm">
      <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1.5 @md:grid-cols-[auto_minmax(3rem,1fr)_auto_auto]">
        <p className="min-w-0 break-words font-medium">{window.label}</p>
        {known && (
          <Progress
            value={value}
            aria-label={`${window.label} remaining`}
            className="col-span-2 row-start-2 gap-0 @md:col-span-1 @md:col-start-2 @md:row-start-1"
          />
        )}
        <p className="col-start-2 row-start-1 text-muted-foreground tabular-nums @md:col-start-3">
          {percent} remaining
        </p>
        {reset && Number.isFinite(reset.getTime()) && (
          <div className="col-span-2 row-start-3 @md:col-span-1 @md:col-start-4 @md:row-start-1">
            <ResetTime reset={reset} />
          </div>
        )}
      </div>
    </div>
  );
}

type DialogState =
  | { status: "idle" }
  | { status: "working" }
  | { status: "error"; message: string };

function ConsoleDialog({
  providerId,
  credentialId,
  credentialName,
  kind,
  onClose,
}: {
  providerId: string;
  credentialId: string;
  credentialName: string;
  kind: "zen" | "openrouter";
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const [state, setState] = useState<DialogState>({ status: "idle" });
  const busy = state.status === "working";
  const error = state.status === "error" ? state.message : null;
  const isZen = kind === "zen";
  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: credentialUsageOptions(providerId, credentialId).queryKey,
    });
  const form = useForm({
    defaultValues: { workspace_id: "", session: "", management_key: "" },
    onSubmit: async ({ value }) => {
      setState({ status: "working" });
      try {
        if (isZen) {
          await saveConsoleSession(providerId, credentialId, {
            workspace_id: value.workspace_id,
            session: value.session,
          });
        } else {
          await saveOpenrouterManagementKey(providerId, credentialId, {
            management_key: value.management_key,
          });
        }
        await refresh();
        onClose();
      } catch {
        setState({
          status: "error",
          message: isZen
            ? "Could not save the console session. Check the workspace ID and try again."
            : "Could not save the management key. Try again.",
        });
      }
    },
  });
  async function remove() {
    setState({ status: "working" });
    try {
      if (isZen) {
        await deleteConsoleSession(providerId, credentialId);
      } else {
        await deleteOpenrouterManagementKey(providerId, credentialId);
      }
      await refresh();
      onClose();
    } catch {
      setState({
        status: "error",
        message: isZen
          ? "Could not remove the console session. Try again."
          : "Could not remove the management key. Try again.",
      });
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
            <DialogTitle>
              {isZen ? "Zen console" : "OpenRouter management key"} for{" "}
              {credentialName}
            </DialogTitle>
            <DialogDescription>
              {isZen
                ? "Associate this credential with your Zen Console workspace to view its balance. Enter the workspace ID and the value of the __Host-console_session cookie, not an API key. The session is stored encrypted, expires, and may need to be replaced."
                : "Enter a separate OpenRouter management key to view the account's remaining credits. An inference API key cannot access account credits. The management key is stored encrypted."}
            </DialogDescription>
          </DialogHeader>
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          {isZen ? (
            <>
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
                      onChange={(event) =>
                        field.handleChange(event.target.value)
                      }
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
                      onChange={(event) =>
                        field.handleChange(event.target.value)
                      }
                    />
                  </Label>
                )}
              </form.Field>
            </>
          ) : (
            <form.Field name="management_key">
              {(field) => (
                <Label className="block text-sm font-medium">
                  Management key
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
          )}
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={busy}>
              {busy
                ? "Working…"
                : isZen
                  ? "Save session"
                  : "Save management key"}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => void remove()}
            >
              {isZen ? "Remove session" : "Remove management key"}
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
  const usage = useQuery(
    credentialUsageOptions(providerId, credentialId, connected),
  );

  return (
    <div
      className={
        kind !== "zen" && kind !== "openrouter"
          ? "w-full min-w-0 space-y-2 sm:w-[32rem] sm:max-w-full"
          : "min-w-0 space-y-2 sm:max-w-sm"
      }
      aria-label={`Usage for ${credentialName}`}
    >
      {(kind === "zen" || kind === "openrouter") && (
        <Button
          size="sm"
          variant="outline"
          onClick={() => setConfiguring(true)}
        >
          {kind === "zen" ? "Configure console" : "Configure management key"}
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
          <div className="space-y-3">
            {usage.data.windows.map((window, index) => (
              <WindowUsage key={`${window.label}-${index}`} window={window} />
            ))}
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            No quota windows available.
          </p>
        ))}
      {connected && !usage.isError && usage.data?.kind === "grouped_quota" && (
        <div className="space-y-4">
          {usage.data.groups.map((group, index) => (
            <section
              key={`${group.label}-${index}`}
              role="group"
              aria-label={group.label}
              className="space-y-2"
            >
              <h3 className="text-sm font-semibold">{group.label}</h3>
              <div className="space-y-3">
                {group.windows.map((window, windowIndex) => (
                  <WindowUsage
                    key={`${window.label}-${windowIndex}`}
                    window={window}
                  />
                ))}
              </div>
            </section>
          ))}
        </div>
      )}
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
      {configuring && (kind === "zen" || kind === "openrouter") && (
        <ConsoleDialog
          providerId={providerId}
          credentialId={credentialId}
          credentialName={credentialName}
          kind={kind}
          onClose={() => setConfiguring(false)}
        />
      )}
    </div>
  );
}
