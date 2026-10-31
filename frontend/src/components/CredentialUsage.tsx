import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { message } from "../server/api/client";
import {
  credentialUsageOptions,
  type QuotaWindow,
} from "../server/api/providers";
import { Button } from "./ui/button";
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
      <TooltipTrigger className="whitespace-nowrap text-left text-muted-foreground underline decoration-dotted underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2">
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
    <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1.5 text-sm @md:col-span-4 @md:grid-cols-subgrid">
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
  );
}

export function CredentialUsage({
  providerId,
  credentialId,
  credentialName,
  connected,
}: {
  providerId: string;
  credentialId: string;
  credentialName: string;
  connected: boolean;
}) {
  const usage = useQuery(
    credentialUsageOptions(providerId, credentialId, connected),
  );

  return (
    <div
      className="@container w-full min-w-0 space-y-2"
      aria-label={`Usage for ${credentialName}`}
    >
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
          <div className="space-y-3 @md:grid @md:grid-cols-[4.5rem_minmax(0,1fr)_8rem_minmax(11rem,max-content)] @md:gap-x-3">
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
        <div className="space-y-4 @md:grid @md:grid-cols-[4.5rem_minmax(0,1fr)_8rem_minmax(11rem,max-content)] @md:gap-x-3">
          {usage.data.groups.map((group, index) => (
            <section
              key={`${group.label}-${index}`}
              role="group"
              aria-label={group.label}
              className="space-y-2 @md:col-span-4 @md:grid @md:grid-cols-subgrid"
            >
              <h3 className="text-sm font-semibold @md:col-span-4">
                {group.label}
              </h3>
              <div className="space-y-3 @md:col-span-4 @md:grid @md:grid-cols-subgrid">
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
    </div>
  );
}
