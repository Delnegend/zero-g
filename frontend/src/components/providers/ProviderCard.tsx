import { useQuery } from "@tanstack/react-query";
import { Pencil, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  credentialsOptions,
  type Provider,
  type ProviderKind,
} from "@/server/api/providers";
import { Credentials } from "./Credentials";
import { ProviderSignIn } from "./ProviderSignIn";

export function ProviderCard({
  provider,
  kind,
  onEdit,
  onDelete,
  deleting,
}: {
  provider: Provider;
  kind?: ProviderKind;
  onEdit: () => void;
  onDelete: () => void;
  deleting: boolean;
}) {
  const credentials = useQuery(credentialsOptions(provider.id));
  const hasCredentials = (credentials.data?.credentials.length ?? 0) > 0;
  const cannotDelete =
    credentials.isPending ||
    credentials.isFetching ||
    credentials.isError ||
    hasCredentials;
  return (
    <Card className="mt-6 p-5">
      <ProviderSignIn provider={provider}>
        {({ busy, loginBusy, start }) => (
          <>
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <h2 className="break-words text-lg font-semibold">
                  {provider.name}
                </h2>
                <p className="mt-1 break-all text-sm text-muted-foreground">
                  {kind?.label ?? provider.kind} · {provider.id}
                </p>
              </div>
              <div className="flex shrink-0 gap-2">
                {kind &&
                  ((kind.api_key &&
                    provider.kind !== "zen" &&
                    provider.kind !== "openrouter") ||
                    kind.fixed_name === null) && (
                    <Button
                      variant="outline"
                      size="icon"
                      aria-label={`Edit ${provider.name}`}
                      disabled={busy || deleting}
                      onClick={onEdit}
                    >
                      <Pencil />
                    </Button>
                  )}
                <Button
                  variant="destructive"
                  size="icon"
                  aria-label={
                    deleting
                      ? `Deleting ${provider.name}`
                      : `Delete ${provider.name}`
                  }
                  disabled={busy || deleting || cannotDelete}
                  onClick={onDelete}
                >
                  <Trash2 />
                </Button>
              </div>
            </div>
            {hasCredentials && (
              <p className="mt-3 text-sm text-muted-foreground">
                Remove all credentials before deleting this provider.
              </p>
            )}
            <Credentials
              provider={provider}
              kind={kind}
              onLogin={start}
              loginBusy={loginBusy}
            />
            {kind?.api_key &&
              provider.kind !== "zen" &&
              provider.kind !== "openrouter" && (
                <p className="mt-2 break-all text-sm text-muted-foreground">
                  Upstream: {provider.base_url}
                </p>
              )}
          </>
        )}
      </ProviderSignIn>
    </Card>
  );
}
