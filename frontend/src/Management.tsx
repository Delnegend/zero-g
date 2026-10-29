import { useEffect, useRef, useState } from "react";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "./components/ui/button";
import { Input } from "./components/ui/input";
import { Textarea } from "./components/ui/textarea";
import { Checkbox } from "./components/ui/checkbox";
import { Alert, AlertDescription } from "./components/ui/alert";
import { Card } from "./components/ui/card";
import { FormSkeleton, RowsSkeleton } from "./components/Loading";
import { Label } from "./components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "./components/ui/dialog";
import {
  getSettings,
  updateSettings,
  type SettingsUpdate,
} from "./api/settings";
import { message, request } from "./api/client";
import { getProviders } from "./api/providers";
import { ProvidersPage } from "./ProvidersPage";

type VirtualKey = {
  id: string;
  name: string;
  providers: string[];
  models: string[];
  created_at: number;
};

const inputClass =
  "mt-1 h-9 w-full rounded-lg border border-input bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";
const panelClass = "p-5";

function SettingsPage() {
  const draftStarted = useRef(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const saveController = useRef<AbortController | null>(null);

  const settingsQuery = useQuery({
    queryKey: ["settings"],
    retry: false,
    queryFn: ({ signal }) => getSettings(signal),
  });
  const queryClient = useQueryClient();
  const saveMutation = useMutation({
    gcTime: 0,
    mutationFn: ({
      body,
      signal,
    }: {
      body: SettingsUpdate;
      signal: AbortSignal;
    }) => updateSettings(body, signal),
  });

  const settings = settingsQuery.data ?? null;
  const displayError =
    error || (settingsQuery.error ? message(settingsQuery.error) : "");

  useEffect(() => () => saveController.current?.abort(), []);

  const form = useForm({
    defaultValues: {
      log_retention_days: settings?.log_retention_days ?? 1,
      oauth_client_id: settings?.oauth_client_id ?? "",
      oauthSecret: "",
      clearOauth: false,
    },
    onSubmit: ({ value }) => save(value),
  });

  useEffect(() => {
    if (settings && !draftStarted.current) {
      form.setFieldValue("log_retention_days", settings.log_retention_days);
      form.setFieldValue("oauth_client_id", settings.oauth_client_id);
    }
  }, [settings, form]);

  async function save(value: {
    log_retention_days: number;
    oauth_client_id: string;
    oauthSecret: string;
    clearOauth: boolean;
  }) {
    if (!settings) return;
    saveController.current?.abort();
    const controller = new AbortController();
    saveController.current = controller;
    setBusy(true);
    setError("");
    setSaved(false);
    try {
      const body: SettingsUpdate = {
        log_retention_days: value.log_retention_days,
        oauth_client_id: value.oauth_client_id,
      };
      if (value.clearOauth || value.oauthSecret)
        body.oauth_client_secret = value.clearOauth ? "" : value.oauthSecret;
      const updated = await saveMutation.mutateAsync({
        body,
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      await queryClient.cancelQueries({ queryKey: ["settings"] });
      if (controller.signal.aborted) return;
      const current = form.state.values;
      const logRetention =
        current.log_retention_days === value.log_retention_days
          ? updated.log_retention_days
          : current.log_retention_days;
      const oauthClientId =
        current.oauth_client_id === value.oauth_client_id
          ? updated.oauth_client_id
          : current.oauth_client_id;
      draftStarted.current =
        logRetention !== updated.log_retention_days ||
        oauthClientId !== updated.oauth_client_id;
      queryClient.setQueryData(["settings"], updated);
      form.setFieldValue("log_retention_days", logRetention);
      form.setFieldValue("oauth_client_id", oauthClientId);
      if (current.oauthSecret === value.oauthSecret)
        form.setFieldValue("oauthSecret", "");
      if (current.clearOauth === value.clearOauth)
        form.setFieldValue("clearOauth", false);
      setSaved(true);
    } catch (err) {
      if (!controller.signal.aborted) setError(message(err));
    } finally {
      saveMutation.reset();
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return (
    <main className="mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Settings</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Configure the gateway and provider credentials. Some changes may require
        a restart.
      </p>
      {displayError && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{displayError}</AlertDescription>
        </Alert>
      )}
      {saved && (
        <p role="status" className="mt-5 text-sm">
          Settings saved.
        </p>
      )}
      {settingsQuery.isPending && <FormSkeleton label="Loading settings" />}
      {settings && (
        <Card className="mt-6 p-5">
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void form.handleSubmit();
            }}
            className="grid gap-5"
          >
            <form.Field name="log_retention_days">
              {(field) => (
                <Label className="block text-sm font-medium">
                  Log retention (days)
                  <Input
                    className={inputClass}
                    type="number"
                    min={1}
                    max={36500}
                    required
                    value={field.state.value}
                    onBlur={field.handleBlur}
                    onChange={(e) => {
                      draftStarted.current = true;
                      field.handleChange(Number(e.target.value));
                    }}
                  />
                </Label>
              )}
            </form.Field>
            <form.Field name="oauth_client_id">
              {(field) => (
                <Label className="block text-sm font-medium">
                  Google OAuth client ID
                  <Input
                    className={inputClass}
                    value={field.state.value}
                    onBlur={field.handleBlur}
                    onChange={(e) => {
                      draftStarted.current = true;
                      field.handleChange(e.target.value);
                    }}
                  />
                </Label>
              )}
            </form.Field>
            <form.Subscribe selector={(state) => state.values.clearOauth}>
              {(clearOauth) => (
                <form.Field name="oauthSecret">
                  {(field) => (
                    <Label className="block text-sm font-medium">
                      Google OAuth client secret{" "}
                      <span className="font-normal text-muted-foreground">
                        (
                        {settings.oauth_client_secret_set
                          ? "configured; leave blank to retain"
                          : "not configured"}
                        )
                      </span>
                      <Input
                        className={inputClass}
                        type="password"
                        autoComplete="new-password"
                        disabled={clearOauth}
                        value={field.state.value}
                        onBlur={field.handleBlur}
                        onChange={(e) => field.handleChange(e.target.value)}
                      />
                    </Label>
                  )}
                </form.Field>
              )}
            </form.Subscribe>
            {settings.oauth_client_secret_set && (
              <form.Field name="clearOauth">
                {(field) => (
                  <Label htmlFor="clear-oauth" className="text-sm">
                    <Checkbox
                      id="clear-oauth"
                      checked={field.state.value}
                      onCheckedChange={field.handleChange}
                    />
                    Clear Google OAuth client secret
                  </Label>
                )}
              </form.Field>
            )}
            <div>
              <Button type="submit" disabled={busy}>
                {busy ? "Saving…" : "Save settings"}
              </Button>
            </div>
          </form>
        </Card>
      )}
    </main>
  );
}

function KeysPage() {
  const queryClient = useQueryClient();
  const keysQuery = useQuery({
    queryKey: ["virtual-keys"],
    retry: false,
    queryFn: ({ signal }) =>
      request<{ keys: VirtualKey[] }>("/api/keys", { signal }),
  });
  const providersQuery = useQuery({
    queryKey: ["providers"],
    retry: false,
    queryFn: ({ signal }) => getProviders(signal),
  });
  const keys = keysQuery.data?.keys ?? [];
  const createMutation = useMutation({
    gcTime: 0,
    mutationFn: (body: {
      name: string;
      providers: string[];
      models: string[];
    }) =>
      request<VirtualKey & { key: string }>("/api/keys", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      }),
  });
  const revokeMutation = useMutation({
    mutationFn: (id: string) =>
      request(`/api/keys/${encodeURIComponent(id)}`, { method: "DELETE" }),
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
        { queryKey: ["virtual-keys"] },
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
      await queryClient.cancelQueries({ queryKey: ["virtual-keys"] });
      queryClient.setQueryData<{ keys: VirtualKey[] }>(
        ["virtual-keys"],
        (current) =>
          current && {
            ...current,
            keys: current.keys.filter((item) => item.id !== key.id),
          },
      );
      await queryClient.invalidateQueries(
        { queryKey: ["virtual-keys"] },
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
                      (comma or newline separated; blank allows all)
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

export function Management({
  page,
}: {
  page: "settings" | "keys" | "providers";
}) {
  return page === "settings" ? (
    <SettingsPage />
  ) : page === "keys" ? (
    <KeysPage />
  ) : (
    <ProvidersPage />
  );
}
