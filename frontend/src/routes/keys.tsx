import { RowsSkeleton } from "@/components/Loading";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { toast, Toaster } from "@/components/ui/toast";
import { Page } from "@/routes/__root";
import { message } from "@/server/api/client";
import {
  createKey,
  keysOptions,
  revokeKey as revokeKeyRequest,
  unrevokeKey as unrevokeKeyRequest,
  updateKey,
  type VirtualKey,
} from "@/server/api/keys";
import { modelCatalogQueryOptions } from "@/server/api/models";
import { providersOptions } from "@/server/api/providers";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Ban, Pencil } from "lucide-react";
import { useState } from "react";
import {
  expiration,
  ExpirationStatus,
  KeyForm,
  localDateTime,
} from "@/components/KeyForm";

export const Route = createFileRoute("/keys")({
  head: () => ({ meta: [{ title: "Virtual keys · Zero-G" }] }),
  component: KeysPage,
});

export function KeysPage() {
  const queryClient = useQueryClient();
  const keysQuery = useQuery(keysOptions());
  const providersQuery = useQuery(providersOptions());
  const catalogQuery = useQuery(modelCatalogQueryOptions());
  const createMutation = useMutation({ gcTime: 0, mutationFn: createKey });
  const revokeMutation = useMutation({ mutationFn: revokeKeyRequest });
  const unrevokeMutation = useMutation({ mutationFn: unrevokeKeyRequest });
  const updateMutation = useMutation({
    mutationFn: ({
      id,
      name,
      providers,
      models,
      expires_at,
    }: {
      id: string;
      name?: string;
      providers?: string[];
      models?: string[];
      expires_at?: number | null;
    }) => updateKey(id, { name, providers, models, expires_at }),
  });
  const [newKey, setNewKey] = useState("");
  const [error, setError] = useState("");
  const [success, setSuccess] = useState("");
  const [mutationSuccess, setMutationSuccess] = useState(false);
  const [busy, setBusy] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [revokeKey, setRevokeKey] = useState<VirtualKey | null>(null);
  const [editKey, setEditKey] = useState<VirtualKey | null>(null);
  const refreshError =
    (success || mutationSuccess) && keysQuery.error
      ? message(keysQuery.error)
      : "";
  const keys = keysQuery.data?.keys ?? [];
  const catalogErrors =
    catalogQuery.data?.providers.filter((provider) => provider.error) ?? [];
  const optionsUnavailable =
    providersQuery.isPending ||
    !!providersQuery.error ||
    catalogQuery.isPending;
  async function refreshKeys() {
    await queryClient.invalidateQueries({ queryKey: keysOptions().queryKey });
  }
  async function create(value: {
    name: string;
    providers: string[];
    models: string[];
    expiresAt: string;
  }) {
    setBusy(true);
    setError("");
    setSuccess("");
    setNewKey("");
    try {
      const result = await createMutation.mutateAsync({
        name: value.name,
        providers: value.providers,
        models: value.models,
        expires_at: expiration(value.expiresAt),
      });
      setNewKey(result.key);
      setSuccess("Key created successfully.");
      setMutationSuccess(true);
      await refreshKeys();
    } catch (err) {
      setError(message(err));
    } finally {
      createMutation.reset();
      setBusy(false);
    }
  }
  async function revoke(key: VirtualKey) {
    setBusy(true);
    setError("");
    setSuccess("");
    try {
      await revokeMutation.mutateAsync(key.id);
      setRevokeKey(null);
      setMutationSuccess(true);
      await refreshKeys();
      const toastId = toast.add({
        type: "success",
        title: "Key revoked successfully.",
        actionProps: {
          children: "Undo",
          onClick: async () => {
            toast.close(toastId);
            await unrevoke(key);
          },
        },
      });
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }
  async function unrevoke(key: VirtualKey) {
    setBusy(true);
    setError("");
    try {
      await unrevokeMutation.mutateAsync(key.id);
      setMutationSuccess(true);
      await refreshKeys();
      toast.add({
        type: "success",
        title: "Key restored successfully.",
      });
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }
  async function saveKey(
    key: VirtualKey,
    value: {
      name: string;
      providers: string[];
      models: string[];
      expiresAt: string;
    },
  ) {
    setBusy(true);
    setError("");
    setSuccess("");
    try {
      await updateMutation.mutateAsync({
        id: key.id,
        name: value.name,
        providers: value.providers,
        models: value.models,
        expires_at: expiration(value.expiresAt),
      });
      setEditKey(null);
      setSuccess("Key updated successfully.");
      setMutationSuccess(true);
      await refreshKeys();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Page
      title="Virtual keys"
      description="Create keys for inference access and restrict their providers and models."
      actions={
        <Button
          onClick={() => {
            setError("");
            setCreateOpen(true);
          }}
        >
          Create key
        </Button>
      }
    >
      {success && !createOpen && (
        <p role="status" className="mt-5">
          {success}
        </p>
      )}
      {(refreshError || keysQuery.error) && !createOpen && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>
            {refreshError
              ? `The key list could not be refreshed and may be out of date. ${refreshError}`
              : message(keysQuery.error)}{" "}
            <button
              type="button"
              className="font-medium underline underline-offset-4"
              onClick={() => void refreshKeys()}
            >
              Retry key list
            </button>
          </AlertDescription>
        </Alert>
      )}
      <Dialog
        open={createOpen}
        onOpenChange={(open) => {
          if (!open && (busy || newKey)) return;
          setCreateOpen(open);
        }}
      >
        <DialogContent
          className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
          showCloseButton={!busy && !newKey}
        >
          <DialogHeader>
            <DialogTitle>
              {newKey ? "Copy your new key now" : "Create key"}
            </DialogTitle>
            <DialogDescription>
              {newKey
                ? "It will not be shown again."
                : "Create an inference key and choose its provider and model restrictions."}
            </DialogDescription>
          </DialogHeader>
          {newKey && <p role="status">{success}</p>}
          {refreshError && (
            <Alert variant="destructive">
              <AlertDescription>
                The key list could not be refreshed and may be out of date.{" "}
                {refreshError}{" "}
                <button
                  type="button"
                  className="font-medium underline underline-offset-4"
                  onClick={() => void refreshKeys()}
                >
                  Retry key list
                </button>
              </AlertDescription>
            </Alert>
          )}
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          {!newKey && providersQuery.error && (
            <Alert variant="destructive">
              <AlertDescription>
                Could not load provider options. Key creation is disabled until
                they are available. {message(providersQuery.error)}
              </AlertDescription>
            </Alert>
          )}
          {!newKey && (catalogQuery.error || catalogErrors.length > 0) && (
            <Alert variant="destructive">
              <AlertDescription>
                Some model options could not be loaded. Only listed models can
                be selected; selecting none allows all models.{" "}
                {catalogQuery.error
                  ? message(catalogQuery.error)
                  : catalogErrors
                      .map((provider) => `${provider.id}: ${provider.error}`)
                      .join(", ")}
              </AlertDescription>
            </Alert>
          )}
          {newKey ? (
            <div role="status">
              <code className="block break-all select-all">{newKey}</code>
              <Button
                className="mt-3"
                variant="outline"
                disabled={busy}
                onClick={() => {
                  setNewKey("");
                  setError("");
                  setCreateOpen(false);
                }}
              >
                Dismiss
              </Button>
            </div>
          ) : (
            <KeyForm
              defaultValues={{
                name: "",
                providers: [],
                models: [],
                expiresAt: "",
              }}
              submitLabel="Create key"
              busy={busy}
              optionsUnavailable={optionsUnavailable}
              providers={providersQuery.data?.providers ?? []}
              catalogProviders={catalogQuery.data?.providers ?? []}
              onCancel={() => setCreateOpen(false)}
              onSubmit={(value) => void create(value)}
            />
          )}
        </DialogContent>
      </Dialog>
      <Card className="mt-6 p-5">
        <h2 className="text-lg font-semibold">Existing keys</h2>
        {keysQuery.isPending ? (
          <div className="mt-4">
            <RowsSkeleton label="Loading virtual keys" />
          </div>
        ) : keysQuery.error ? null : keys.length === 0 ? (
          <p className="mt-4 text-sm text-muted-foreground">
            No keys created yet.
          </p>
        ) : (
          <ul className="divide-y">
            {keys.map((key) => (
              <li
                key={key.id}
                className="flex items-center justify-between gap-4 py-4"
              >
                <div className="min-w-0 flex-1">
                  <h3 className="font-medium break-all">{key.name}</h3>
                  <p className="mt-1 text-sm break-all text-muted-foreground">
                    Providers:{" "}
                    {key.providers.length ? key.providers.join(", ") : "All"} ·
                    Models: {key.models.length ? key.models.join(", ") : "All"}
                  </p>
                  <div className="mt-1 text-sm text-muted-foreground">
                    {key.revoked_at != null ? (
                      "Revoked"
                    ) : (
                      <ExpirationStatus expiresAt={key.expires_at} />
                    )}
                  </div>
                </div>
                <div className="flex shrink-0 gap-2">
                  <Button
                    variant="outline"
                    size="icon"
                    disabled={busy || key.revoked_at != null}
                    tooltip="Edit key"
                    aria-label={`Edit ${key.name}`}
                    onClick={() => {
                      setError("");
                      setEditKey(key);
                    }}
                  >
                    <Pencil />
                  </Button>
                  <Button
                    variant="outline"
                    size="icon"
                    disabled={busy || key.revoked_at != null}
                    tooltip="Revoke key"
                    aria-label={`Revoke ${key.name}`}
                    onClick={() => {
                      setError("");
                      setRevokeKey(key);
                    }}
                  >
                    <Ban />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </Card>
      {editKey && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open && !busy) setEditKey(null);
          }}
        >
          <DialogContent
            className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
            showCloseButton={!busy}
          >
            <DialogHeader>
              <DialogTitle>Edit key</DialogTitle>
              <DialogDescription>
                Update permissions and expiration for “{editKey.name}”.
              </DialogDescription>
            </DialogHeader>
            {error && (
              <Alert variant="destructive">
                <AlertDescription>{error}</AlertDescription>
              </Alert>
            )}
            <KeyForm
              key={editKey.id}
              defaultValues={{
                name: editKey.name,
                providers: editKey.providers,
                models: editKey.models,
                expiresAt: localDateTime(editKey.expires_at),
              }}
              submitLabel="Save key"
              busy={busy}
              optionsUnavailable={optionsUnavailable}
              providers={providersQuery.data?.providers ?? []}
              catalogProviders={catalogQuery.data?.providers ?? []}
              onCancel={() => setEditKey(null)}
              onSubmit={(value) => void saveKey(editKey, value)}
            />
          </DialogContent>
        </Dialog>
      )}
      <Dialog
        open={revokeKey !== null}
        onOpenChange={(open) => {
          if (!open && !busy) setRevokeKey(null);
        }}
      >
        <DialogContent showCloseButton={!busy}>
          <DialogHeader>
            <DialogTitle>Revoke key?</DialogTitle>
            <DialogDescription>
              Revoke key “{revokeKey?.name}”? This cannot be undone.
            </DialogDescription>
          </DialogHeader>
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              disabled={busy}
              onClick={() => setRevokeKey(null)}
            >
              Cancel
            </Button>
            <Button
              variant="destructive"
              disabled={busy}
              onClick={() => {
                if (revokeKey) void revoke(revokeKey);
              }}
            >
              Revoke
            </Button>
          </div>
        </DialogContent>
      </Dialog>
      <Toaster />
    </Page>
  );
}
