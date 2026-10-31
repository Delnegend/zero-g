import { createFileRoute } from "@tanstack/react-router";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { message } from "@/server/api/client";
import { Textarea } from "@/components/ui/textarea";
import { Checkbox } from "@/components/ui/checkbox";
import { RowsSkeleton } from "@/components/Loading";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  createKey,
  keysOptions,
  revokeKey as revokeKeyRequest,
  type VirtualKey,
} from "@/server/api/keys";
import { providersOptions } from "@/server/api/providers";

export const Route = createFileRoute("/keys")({
  head: () => ({ meta: [{ title: "Virtual keys · Zero-G" }] }),
  component: KeysPage,
});

const inputClass =
  "mt-1 h-9 w-full rounded-lg border border-input bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";
const panelClass = "p-5";

export function KeysPage() {
  const queryClient = useQueryClient();
  const keysQuery = useQuery(keysOptions());
  const providersQuery = useQuery(providersOptions());
  const keys = keysQuery.data?.keys ?? [];
  const createMutation = useMutation({
    gcTime: 0,
    mutationFn: createKey,
  });
  const revokeMutation = useMutation({
    mutationFn: revokeKeyRequest,
  });
  const [newKey, setNewKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [revokeKey, setRevokeKey] = useState<VirtualKey | null>(null);
  const loading = keysQuery.isPending;
  const displayError =
    (keysQuery.error ? message(keysQuery.error) : "") ||
    (providersQuery.error ? message(providersQuery.error) : "");
  const form = useForm({
    defaultValues: { name: "", providers: [] as string[], models: "" },
    onSubmit: ({ value }) => create(value),
  });

  async function create(value: {
    name: string;
    providers: string[];
    models: string;
  }) {
    setBusy(true);
    setError("");
    setNewKey("");
    try {
      const result = await createMutation.mutateAsync({
        name: value.name,
        providers: value.providers,
        models: value.models
          .split(/[\n,]/)
          .map((model) => model.trim())
          .filter(Boolean),
      });
      setNewKey(result.key);
      form.reset();
      await queryClient.invalidateQueries(
        { queryKey: keysOptions().queryKey },
        { throwOnError: true },
      );
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
    try {
      await revokeMutation.mutateAsync(key.id);
      await queryClient.cancelQueries({ queryKey: keysOptions().queryKey });
      queryClient.setQueryData<{ keys: VirtualKey[] }>(
        keysOptions().queryKey,
        (current) =>
          current && {
            ...current,
            keys: current.keys.filter((item) => item.id !== key.id),
          },
      );
      await queryClient.invalidateQueries(
        { queryKey: keysOptions().queryKey },
        { throwOnError: true },
      );
      setRevokeKey(null);
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }
  return (
    <main className="mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Virtual keys</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Create keys for inference access and restrict their providers and
        models.
      </p>
      {displayError && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{displayError}</AlertDescription>
        </Alert>
      )}
      <Button
        className="mt-6"
        onClick={() => {
          setError("");
          setCreateOpen(true);
        }}
      >
        Create key
      </Button>
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
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          {providersQuery.error && !newKey && (
            <Alert variant="destructive">
              <AlertDescription>
                {message(providersQuery.error)}
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
            <form
              onSubmit={(event) => {
                event.preventDefault();
                void form.handleSubmit();
              }}
              className="grid gap-5"
            >
              <form.Field name="name">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Name
                    <Input
                      className={inputClass}
                      disabled={busy}
                      required
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(e) => field.handleChange(e.target.value)}
                    />
                  </Label>
                )}
              </form.Field>
              <form.Field name="providers">
                {(field) => {
                  const providers = field.state.value;
                  const missingProviders = providers.filter(
                    (id) =>
                      !providersQuery.data?.providers.some(
                        (provider) => provider.id === id,
                      ),
                  );
                  return (
                    <fieldset>
                      <legend className="text-sm font-medium">Providers</legend>
                      <p className="mt-1 text-xs text-muted-foreground">
                        Select none to allow all providers.
                      </p>
                      <div className="mt-2 flex flex-wrap gap-4">
                        {(providersQuery.data?.providers ?? []).map(
                          (provider) => (
                            <Label
                              key={provider.id}
                              htmlFor={`provider-${provider.id}`}
                              className="text-sm"
                            >
                              <Checkbox
                                id={`provider-${provider.id}`}
                                disabled={busy}
                                checked={providers.includes(provider.id)}
                                onCheckedChange={(checked) =>
                                  field.handleChange(
                                    checked
                                      ? [...providers, provider.id]
                                      : providers.filter(
                                          (item) => item !== provider.id,
                                        ),
                                  )
                                }
                              />
                              {provider.name} ({provider.id})
                            </Label>
                          ),
                        )}
                        {missingProviders.map((id) => (
                          <Label
                            key={id}
                            htmlFor={`provider-${id}`}
                            className="text-sm"
                          >
                            <Checkbox
                              id={`provider-${id}`}
                              disabled={busy}
                              checked
                              onCheckedChange={() =>
                                field.handleChange(
                                  providers.filter((item) => item !== id),
                                )
                              }
                            />
                            {id} (no longer available; deselect)
                          </Label>
                        ))}
                      </div>
                    </fieldset>
                  );
                }}
              </form.Field>
              <form.Field name="models">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Models{" "}
                    <span className="font-normal text-muted-foreground">
                      (upstream model IDs, comma or newline separated; blank
                      allows all)
                    </span>
                    <Textarea
                      className={`${inputClass} h-24 py-2`}
                      disabled={busy}
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(e) => field.handleChange(e.target.value)}
                    />
                  </Label>
                )}
              </form.Field>
              <div className="flex flex-wrap gap-2">
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy}
                  onClick={() => setCreateOpen(false)}
                >
                  Cancel
                </Button>
                <form.Subscribe selector={(state) => state.values.providers}>
                  {(providers) => (
                    <Button
                      type="submit"
                      disabled={
                        busy ||
                        providersQuery.isPending ||
                        !!providersQuery.error ||
                        providers.some(
                          (id) =>
                            !providersQuery.data?.providers.some(
                              (provider) => provider.id === id,
                            ),
                        )
                      }
                    >
                      {busy ? "Working…" : "Create key"}
                    </Button>
                  )}
                </form.Subscribe>
              </div>
            </form>
          )}
        </DialogContent>
      </Dialog>
      <Card className={`${panelClass} mt-6`}>
        <h2 className="text-lg font-semibold">Existing keys</h2>
        {loading ? (
          <div className="mt-4">
            <RowsSkeleton label="Loading virtual keys" />
          </div>
        ) : keysQuery.error ? null : keys.length === 0 ? (
          <p className="mt-4 text-sm text-muted-foreground">
            No keys created yet.
          </p>
        ) : (
          <ul className="mt-4 divide-y">
            {keys.map((key) => (
              <li
                key={key.id}
                className="flex flex-wrap items-center justify-between gap-4 py-4"
              >
                <div>
                  <h3 className="font-medium">{key.name}</h3>
                  <p className="mt-1 break-all text-sm text-muted-foreground">
                    Providers:{" "}
                    {key.providers.length ? key.providers.join(", ") : "All"} ·
                    Models: {key.models.length ? key.models.join(", ") : "All"}
                  </p>
                </div>
                <Button
                  variant="outline"
                  disabled={busy}
                  onClick={() => {
                    setError("");
                    setRevokeKey(key);
                  }}
                >
                  Revoke / delete
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Card>
      <Dialog
        open={revokeKey !== null}
        onOpenChange={(open) => {
          if (!open && !busy) setRevokeKey(null);
        }}
      >
        <DialogContent
          className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
          showCloseButton={!busy}
        >
          <DialogHeader>
            <DialogTitle>Revoke and delete key?</DialogTitle>
            <DialogDescription>
              Revoke and delete key “{revokeKey?.name}”? This cannot be undone.
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
              {busy ? "Working…" : "Revoke / delete"}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </main>
  );
}
