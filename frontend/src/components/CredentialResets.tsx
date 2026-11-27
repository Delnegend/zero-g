import { useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { formatCountdown } from "@/lib/date";
import { message } from "../server/api/client";
import {
  credentialResetsKey,
  credentialResetsOptions,
  credentialUsageKey,
  redeemCredentialReset,
  type ResetCredit,
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
import { toast } from "./ui/toast";
import { Tooltip, TooltipContent, TooltipTrigger } from "./ui/tooltip";

/** Provider kinds whose accounts can hold banked resets. */
export const RESET_PROVIDERS = ["codex", "claude"] as const;

function Expiry({ expiresAt }: { expiresAt: Date }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 60_000);
    return () => clearInterval(timer);
  }, []);
  const minutes = Math.ceil((expiresAt.getTime() - now) / 60_000);
  return (
    <Tooltip>
      <TooltipTrigger className="text-left whitespace-nowrap underline decoration-dotted underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2">
        {minutes > 0 ? `Expires in ${formatCountdown(minutes)}` : "Expired"}
      </TooltipTrigger>
      <TooltipContent>{expiresAt.toLocaleString()}</TooltipContent>
    </Tooltip>
  );
}

function ResetRow({
  credit,
  onUse,
  disabled,
}: {
  credit: ResetCredit;
  onUse: () => void;
  disabled: boolean;
}) {
  const expiresAt =
    credit.expires_at === null ? null : new Date(credit.expires_at * 1000);
  return (
    <li className="flex min-w-0 flex-wrap items-center justify-between gap-x-3 gap-y-2 text-sm">
      <div className="min-w-0">
        <p className="font-medium wrap-break-word">
          {credit.label}
          {credit.remaining > 1 && (
            <span className="font-normal text-muted-foreground">
              {" "}
              · {credit.remaining} left
            </span>
          )}
        </p>
        <p className="text-muted-foreground">
          {expiresAt && Number.isFinite(expiresAt.getTime()) ? (
            <Expiry expiresAt={expiresAt} />
          ) : (
            "No expiry date"
          )}
          {credit.note && <> · {credit.note}</>}
        </p>
      </div>
      <Button
        variant="outline"
        size="sm"
        disabled={!credit.usable || disabled}
        onClick={onUse}
      >
        Reset now
      </Button>
    </li>
  );
}

/** Banked resets for one credential, each with its expiry date and a button to spend it. */
export function CredentialResets({
  providerId,
  credentialId,
  credentialName,
}: {
  providerId: string;
  credentialId: string;
  credentialName: string;
}) {
  const queryClient = useQueryClient();
  const resets = useQuery(credentialResetsOptions(providerId, credentialId));
  const [target, setTarget] = useState<ResetCredit | null>(null);
  const redeem = useMutation({
    mutationFn: (credit: ResetCredit) =>
      redeemCredentialReset(providerId, credentialId, credit.id),
    onSettled: async () => {
      // Spent or not, the list and the quota windows are out of date now.
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: credentialResetsKey(providerId),
        }),
        queryClient.invalidateQueries({
          queryKey: credentialUsageKey(providerId),
        }),
      ]);
    },
  });

  if (resets.isError) {
    return (
      <p className="text-sm wrap-break-word text-muted-foreground">
        Resets unavailable: {message(resets.error)}
      </p>
    );
  }
  const credits = resets.data?.credits ?? [];
  if (credits.length === 0) {
    return resets.data?.note ? (
      <p className="text-sm text-muted-foreground">{resets.data.note}</p>
    ) : null;
  }

  async function use(credit: ResetCredit) {
    try {
      const result = await redeem.mutateAsync(credit);
      setTarget(null);
      toast.add({
        type: result.redeemed ? "success" : "info",
        title: result.redeemed ? "Reset applied" : "Nothing was reset",
        description: result.message,
      });
    } catch {
      // The dialog shows the error and stays open.
    }
  }

  return (
    <section
      className="space-y-2"
      role="group"
      aria-label={`Resets for ${credentialName}`}
    >
      <h3 className="text-sm font-semibold">Limit resets</h3>
      <ul className="space-y-3">
        {credits.map((credit) => (
          <ResetRow
            key={credit.id}
            credit={credit}
            disabled={redeem.isPending}
            onUse={() => {
              redeem.reset();
              setTarget(credit);
            }}
          />
        ))}
      </ul>
      {target && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open && !redeem.isPending) setTarget(null);
          }}
        >
          <DialogContent showCloseButton={!redeem.isPending}>
            <DialogHeader>
              <DialogTitle>Use this reset?</DialogTitle>
              <DialogDescription>
                “{target.label}” refills the usage windows of {credentialName}{" "}
                right away. It cannot be undone or put back
                {target.expires_at === null
                  ? "."
                  : `, and it would have expired on ${new Date(target.expires_at * 1000).toLocaleDateString()}.`}
              </DialogDescription>
            </DialogHeader>
            {redeem.isError && (
              <Alert variant="destructive">
                <AlertDescription>{message(redeem.error)}</AlertDescription>
              </Alert>
            )}
            <div className="flex gap-2">
              <Button
                disabled={redeem.isPending}
                onClick={() => void use(target)}
              >
                {redeem.isPending ? "Resetting…" : "Use reset"}
              </Button>
              <Button
                variant="outline"
                disabled={redeem.isPending}
                onClick={() => setTarget(null)}
              >
                Cancel
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      )}
    </section>
  );
}
