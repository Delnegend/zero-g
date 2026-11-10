import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Pencil, Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  credentialsOptions,
  type Provider,
  type ProviderKind,
} from "@/server/api/providers";
import { CredentialsAddButton } from "./CredentialsAddButton";
import { CredentialsCard } from "./CredentialsCard";
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
  const [addingCredential, setAddingCredential] = useState(false);
  const credentials = useQuery(credentialsOptions(provider.id));
  const hasCredentials = (credentials.data?.credentials.length ?? 0) > 0;
  const cannotDelete =
    credentials.isPending ||
    credentials.isFetching ||
    credentials.isError ||
    (!provider.keyless && hasCredentials);
  return (
    <Card className="mt-6 p-5">
      <ProviderSignIn provider={provider}>
        {({ busy, loginBusy, start }) => (
          <>
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <h2 className="text-lg font-semibold wrap-break-word">
                  {provider.name}
                </h2>
                {(kind?.label || provider.keyless) && (
                  <p className="mt-1 text-sm break-all text-muted-foreground">
                    {kind?.label
                      ? `${kind.label} · ${provider.id}`
                      : provider.id}
                    {provider.keyless && " · Keyless"}
                  </p>
                )}
              </div>
              <div className="flex shrink-0 gap-2">
                {kind &&
                  ((kind.api_key &&
                    provider.kind !== "zen" &&
                    provider.kind !== "openrouter" &&
                    provider.kind !== "chatjimmy") ||
                    kind.fixed_name === null) && (
                    <Button
                      variant="outline"
                      size="icon"
                      tooltip="Edit provider"
                      label={`Edit ${provider.name}`}
                      disabled={busy || deleting}
                      onClick={onEdit}
                    >
                      <Pencil />
                    </Button>
                  )}
                {!provider.keyless && (
                  <Button
                    variant="outline"
                    size="icon"
                    tooltip="Add credential"
                    label="Add credential"
                    disabled={!kind || busy || loginBusy || deleting}
                    onClick={() => setAddingCredential(true)}
                  >
                    <Plus />
                  </Button>
                )}
                <Button
                  variant="destructive"
                  size="icon"
                  tooltip={
                    !provider.keyless && hasCredentials
                      ? "Remove all credentials before deleting this provider."
                      : "Delete provider"
                  }
                  aria-label={
                    hasCredentials
                      ? `Why ${provider.name} cannot be deleted`
                      : deleting
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
            {!provider.keyless && (
              <CredentialsCard
                provider={provider}
                kind={kind}
                onLogin={start}
                loginBusy={loginBusy}
              />
            )}
            {addingCredential && (
              <CredentialsAddButton
                provider={provider}
                kind={kind}
                editing="new"
                onClose={() => setAddingCredential(false)}
              />
            )}
            {kind?.api_key &&
              provider.kind !== "zen" &&
              provider.kind !== "openrouter" &&
              provider.kind !== "chatjimmy" && (
                <p className="mt-2 text-sm break-all text-muted-foreground">
                  Upstream: {provider.base_url}
                </p>
              )}
          </>
        )}
      </ProviderSignIn>
    </Card>
  );
}
