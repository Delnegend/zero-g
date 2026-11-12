import {
  CredentialUsage,
  CredentialUsageRefreshButton,
  USAGE_PROVIDERS,
} from "@/components/CredentialUsage";
import { CredentialsSkeleton } from "@/components/providers/ProviderSkeletons";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { message } from "@/server/api/client";
import {
  credentialsOptions,
  deleteCredential,
  type Provider,
  type ProviderCredential,
  type ProviderKind,
} from "@/server/api/providers";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Trash2 } from "lucide-react";
import { useState } from "react";
import { CredentialConfiguration } from "./CredentialConfiguration";
import { CredentialsAddButton } from "./CredentialsAddButton";

export function CredentialsCard({
  provider,
  kind,
  onLogin,
  loginBusy,
}: {
  provider: Provider;
  kind?: ProviderKind;
  onLogin: (credential: ProviderCredential) => void;
  loginBusy: boolean;
}) {
  const queryClient = useQueryClient();
  const key = credentialsOptions(provider.id).queryKey;
  const credentials = useQuery(credentialsOptions(provider.id));
  const [editing, setEditing] = useState<ProviderCredential | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<ProviderCredential | null>(
    null,
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  function edit(credential: ProviderCredential) {
    setError("");
    setEditing(credential);
  }

  async function remove() {
    if (!deleteTarget || busy) return;
    setBusy(true);
    setError("");
    try {
      await deleteCredential(provider.id, deleteTarget.id);
      setDeleteTarget(null);
      await queryClient.invalidateQueries({ queryKey: key });
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="flex flex-col gap-4">
      {credentials.isPending && <CredentialsSkeleton />}
      {credentials.error && (
        <Alert variant="destructive">
          <AlertDescription>{message(credentials.error)}</AlertDescription>
        </Alert>
      )}
      {credentials.data?.credentials.length === 0 && (
        <p className="text-sm text-muted-foreground">
          No credentials configured.
        </p>
      )}
      {!credentials.isPending && credentials.data?.credentials.length !== 0 && (
        <div className="grid grid-cols-[repeat(auto-fit,minmax(min(100%,24rem),1fr))] gap-2">
          {credentials.data?.credentials.map((credential) => (
            <div
              key={credential.id}
              className="flex min-w-0 flex-col gap-4 rounded-lg border p-3"
            >
              <div className="min-w-0">
                <p className="font-medium wrap-break-word">
                  {credential.name}
                  {credential.email && (
                    <span className="font-normal text-muted-foreground">
                      {" "}
                      • {credential.email}
                    </span>
                  )}
                  {credential.plan_type && (
                    <span className="font-normal text-muted-foreground">
                      {" "}
                      •{" "}
                      <span className="capitalize">{credential.plan_type}</span>
                    </span>
                  )}
                </p>
                <p className="mt-1 text-sm text-muted-foreground">
                  Weight: {credential.weight} ·{" "}
                  {credential.enabled ? "Enabled" : "Disabled"} ·{" "}
                  {credential.connected ? "Connected" : "Not connected"}
                </p>
              </div>
              {USAGE_PROVIDERS.includes(
                provider.kind as (typeof USAGE_PROVIDERS)[number],
              ) &&
                credential.connected && (
                  <CredentialUsage
                    providerId={provider.id}
                    credentialId={credential.id}
                    credentialName={credential.name}
                    providerKind={provider.kind}
                    connected={credential.connected}
                  />
                )}
              <div className="flex flex-wrap gap-2">
                <Button
                  variant="outline"
                  size="icon"
                  tooltip="Edit credential"
                  label={`Edit ${credential.name}`}
                  disabled={loginBusy}
                  onClick={() => edit(credential)}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="outline"
                  size="icon"
                  tooltip="Delete credential"
                  label={`Delete ${credential.name}`}
                  disabled={loginBusy}
                  onClick={() => {
                    setError("");
                    setDeleteTarget(credential);
                  }}
                >
                  <Trash2 />
                </Button>
                {USAGE_PROVIDERS.includes(
                  provider.kind as (typeof USAGE_PROVIDERS)[number],
                ) && (
                  <CredentialUsageRefreshButton
                    providerId={provider.id}
                    credentialId={credential.id}
                    connected={credential.connected}
                    disabled={loginBusy}
                  />
                )}
                {(provider.kind === "zen" ||
                  provider.kind === "openrouter") && (
                  <CredentialConfiguration
                    providerId={provider.id}
                    credentialId={credential.id}
                    credentialName={credential.name}
                    kind={provider.kind}
                  />
                )}
                {kind?.oauth &&
                  [
                    "agy",
                    "codex",
                    "claude",
                    "cursor",
                    "grok",
                    "grok-build",
                  ].includes(provider.kind) && (
                    <Button
                      disabled={loginBusy || !credential.enabled}
                      onClick={() => onLogin(credential)}
                      variant="secondary"
                    >
                      {credential.connected ? "Reconnect" : "Sign in"}{" "}
                      {
                        (
                          {
                            agy: "Google",
                            codex: "Codex",
                            claude: "Claude Code",
                            cursor: "Cursor",
                            grok: "Grok",
                            "grok-build": "Grok Build",
                          } as Record<string, string>
                        )[provider.kind]
                      }
                    </Button>
                  )}
              </div>
            </div>
          ))}
        </div>
      )}
      {editing && (
        <CredentialsAddButton
          provider={provider}
          kind={kind}
          editing={editing}
          onClose={() => setEditing(null)}
        />
      )}
      {deleteTarget && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open && !busy) setDeleteTarget(null);
          }}
        >
          <DialogContent showCloseButton={!busy}>
            <DialogHeader>
              <DialogTitle>Delete credential?</DialogTitle>
              <DialogDescription>
                Delete “{deleteTarget.name}” and its secret? This cannot be
                undone.
              </DialogDescription>
            </DialogHeader>
            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            <div className="flex gap-2">
              <Button
                variant="destructive"
                disabled={busy}
                onClick={() => void remove()}
              >
                {busy ? "Deleting…" : "Delete credential"}
              </Button>
              <Button
                variant="outline"
                disabled={busy}
                onClick={() => setDeleteTarget(null)}
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
