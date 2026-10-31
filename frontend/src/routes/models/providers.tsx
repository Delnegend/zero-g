import { createFileRoute } from "@tanstack/react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { CardsSkeleton } from "@/components/Loading";
import { ProviderCard } from "@/components/providers/ProviderCard";
import { ProviderForm } from "@/components/providers/ProviderForm";
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
  deleteProvider as deleteProviderRequest,
  providerKindsOptions,
  providersOptions,
  type Provider,
} from "@/server/api/providers";

export const Route = createFileRoute("/models/providers")({
  head: () => ({ meta: [{ title: "Providers · Zero-G" }] }),
  component: ProvidersPage,
});

export function ProvidersPage() {
  const queryClient = useQueryClient();
  const providers = useQuery(providersOptions());
  const kinds = useQuery(providerKindsOptions());
  const remove = useMutation({
    mutationFn: deleteProviderRequest,
  });
  const [editing, setEditing] = useState<Provider | "new" | null>(null);
  const [error, setError] = useState("");
  const [deleting, setDeleting] = useState<string | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<Provider | null>(null);
  const availableKinds = (kinds.data?.kinds ?? []).filter(
    (kind) =>
      kind.fixed_id === null ||
      !providers.data?.providers.some(
        (provider) => provider.kind === kind.value,
      ),
  );

  async function deleteProvider(provider: Provider) {
    if (deleting) return;
    setError("");
    setDeleting(provider.id);
    try {
      await remove.mutateAsync(provider.id);
      await queryClient.cancelQueries({
        queryKey: providersOptions().queryKey,
      });
      queryClient.setQueryData<{ providers: Provider[] }>(
        providersOptions().queryKey,
        (current) =>
          current && {
            providers: current.providers.filter(
              (item) => item.id !== provider.id,
            ),
          },
      );
      if (editing !== "new" && editing?.id === provider.id) setEditing(null);
      await queryClient.invalidateQueries({
        queryKey: providersOptions().queryKey,
      });
      setDeleteTarget(null);
    } catch (err) {
      setError(message(err));
    } finally {
      setDeleting(null);
    }
  }

  return (
    <main className="mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Providers</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Configure providers and their weighted keys or sessions.
      </p>
      {providers.error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{message(providers.error)}</AlertDescription>
        </Alert>
      )}
      {kinds.error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>
            Could not load provider kinds: {message(kinds.error)}
          </AlertDescription>
        </Alert>
      )}
      <Button
        className="mt-6"
        disabled={!!editing || !providers.data || availableKinds.length === 0}
        onClick={() => setEditing("new")}
      >
        Add provider
      </Button>
      {editing && (
        <ProviderForm
          key={editing === "new" ? "new" : editing.id}
          provider={editing === "new" ? undefined : editing}
          kinds={editing === "new" ? availableKinds : (kinds.data?.kinds ?? [])}
          onDone={() => setEditing(null)}
          onCancel={() => setEditing(null)}
        />
      )}
      {providers.isPending ? (
        <CardsSkeleton label="Loading providers" />
      ) : providers.data?.providers.length === 0 ? (
        <p className="mt-6 text-sm text-muted-foreground">
          No providers configured yet.
        </p>
      ) : (
        providers.data?.providers.map((provider) => (
          <ProviderCard
            key={provider.id}
            provider={provider}
            kind={kinds.data?.kinds.find(
              (item) => item.value === provider.kind,
            )}
            onEdit={() => setEditing(provider)}
            onDelete={() => {
              setError("");
              setDeleteTarget(provider);
            }}
            deleting={deleting === provider.id}
          />
        ))
      )}
      {deleteTarget && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open && !deleting) setDeleteTarget(null);
          }}
        >
          <DialogContent
            className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
            showCloseButton={!deleting}
          >
            <DialogHeader>
              <DialogTitle>Delete provider?</DialogTitle>
              <DialogDescription>
                Delete provider “{deleteTarget.name}” ({deleteTarget.id})? This
                cannot be undone, and the ID cannot be reused.
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
                disabled={!!deleting}
                onClick={() => void deleteProvider(deleteTarget)}
              >
                {deleting ? "Deleting…" : "Delete provider"}
              </Button>
              <Button
                variant="outline"
                disabled={!!deleting}
                onClick={() => setDeleteTarget(null)}
              >
                Cancel
              </Button>
            </div>
          </DialogContent>
        </Dialog>
      )}
    </main>
  );
}
