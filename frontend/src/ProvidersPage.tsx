import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { message } from "./server/api/client";
import {
  deleteCredential,
  deleteProvider as deleteProviderRequest,
  finishGoogleSignIn,
  credentialsOptions,
  credentialUsageKey,
  credentialUsageOptions,
  providerKindsOptions,
  providersOptions,
  pollSignIn,
  saveCredential,
  saveProvider,
  startCodexSignIn,
  startGoogleSignIn,
  type CredentialUpdate,
  type DeviceFlow,
  type GoogleFlow,
  type Provider,
  type ProviderCredential,
  type ProviderKind,
} from "./server/api/providers";
import { CredentialUsage } from "./components/CredentialUsage";
import { CardsSkeleton, RowsSkeleton } from "./components/Loading";
import { Alert, AlertDescription } from "./components/ui/alert";
import { Button } from "./components/ui/button";
import { Card } from "./components/ui/card";
import { Checkbox } from "./components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "./components/ui/dialog";
import { Input } from "./components/ui/input";
import { Label } from "./components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "./components/ui/select";

const inputClass =
  "mt-1 h-9 w-full rounded-lg border border-input bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

function ProviderForm({
  provider,
  kinds,
  onDone,
  onCancel,
}: {
  provider?: Provider;
  kinds: ProviderKind[];
  onDone: () => void;
  onCancel: () => void;
}) {
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const queryClient = useQueryClient();
  const save = useMutation({
    gcTime: 0,
    mutationFn: (body: Record<string, string>) =>
      saveProvider(provider?.id, body),
  });
  const form = useForm({
    defaultValues: {
      id: provider?.id ?? "",
      name: provider?.name ?? "",
      kind: provider?.kind ?? kinds[0]?.value ?? "",
      baseUrl: provider?.base_url ?? "",
    },
    onSubmit: async ({ value }) => {
      if (busy) return;
      setError("");
      const kind = kinds.find((item) => item.value === value.kind);
      if (!kind) {
        setError("Provider kind is unavailable.");
        return;
      }
      const body: Record<string, string> = {};
      if (kind.fixed_name === null) body.name = value.name;
      if (kind.api_key) body.base_url = value.baseUrl;
      if (!provider) {
        if (kind.fixed_id === null) body.id = value.id;
        body.kind = value.kind;
      }
      setBusy(true);
      try {
        const updated = await save.mutateAsync(body);
        await queryClient.cancelQueries({
          queryKey: providersOptions().queryKey,
        });
        queryClient.setQueryData<{ providers: Provider[] }>(
          providersOptions().queryKey,
          (current) => ({
            providers: [
              ...(current?.providers ?? []).filter(
                (item) => item.id !== updated.id,
              ),
              updated,
            ],
          }),
        );
        await queryClient.invalidateQueries({
          queryKey: providersOptions().queryKey,
        });
        onDone();
      } catch (err) {
        setError(message(err));
      } finally {
        save.reset();
        setBusy(false);
      }
    },
  });

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onCancel();
      }}
    >
      <DialogContent
        className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
        showCloseButton={!busy}
      >
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void form.handleSubmit();
          }}
          className="grid gap-5"
        >
          <DialogHeader>
            <DialogTitle>
              {provider ? `Edit ${provider.name}` : "Add provider"}
            </DialogTitle>
            <DialogDescription>
              Configure the provider. Credentials are managed separately.
            </DialogDescription>
          </DialogHeader>
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          <form.Subscribe selector={(state) => state.values.kind}>
            {(kind) => {
              const metadata = kinds.find((item) => item.value === kind);
              return (
                <>
                  {metadata?.fixed_id === null && (
                    <form.Field name="id">
                      {(field) => (
                        <Label className="block text-sm font-medium">
                          ID{" "}
                          <span className="font-normal text-muted-foreground">
                            (lowercase letters, digits and hyphens; cannot be
                            changed)
                          </span>
                          <Input
                            className={inputClass}
                            required
                            minLength={1}
                            maxLength={64}
                            pattern="[a-z0-9-]+"
                            disabled={!!provider}
                            value={field.state.value}
                            onBlur={field.handleBlur}
                            onChange={(event) =>
                              field.handleChange(event.target.value)
                            }
                          />
                        </Label>
                      )}
                    </form.Field>
                  )}
                  {metadata?.fixed_name === null && (
                    <form.Field name="name">
                      {(field) => (
                        <Label className="block text-sm font-medium">
                          Name
                          <Input
                            className={inputClass}
                            required
                            value={field.state.value}
                            onBlur={field.handleBlur}
                            onChange={(event) =>
                              field.handleChange(event.target.value)
                            }
                          />
                        </Label>
                      )}
                    </form.Field>
                  )}
                </>
              );
            }}
          </form.Subscribe>
          <div>
            <Label
              htmlFor="provider-kind"
              className="block text-sm font-medium"
            >
              Kind
            </Label>
            <form.Field name="kind">
              {(field) => (
                <Select
                  items={kinds.map(({ value, label }) => ({ value, label }))}
                  disabled={!!provider}
                  value={field.state.value}
                  onValueChange={(value) => {
                    if (!value) return;
                    field.handleChange(value);
                    form.setFieldValue("baseUrl", "");
                  }}
                >
                  <SelectTrigger id="provider-kind" className="mt-1 w-full">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {kinds.map(({ value, label }) => (
                      <SelectItem key={value} value={value}>
                        {label}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </form.Field>
          </div>
          <form.Subscribe selector={(state) => state.values.kind}>
            {(kind) =>
              kinds.find((item) => item.value === kind)?.api_key && (
                <>
                  <form.Field name="baseUrl">
                    {(field) => (
                      <Label className="block text-sm font-medium">
                        Upstream base URL
                        <Input
                          className={inputClass}
                          required={kind !== "go"}
                          type="url"
                          placeholder={
                            kind === "go"
                              ? "https://opencode.ai/inference/go/openai/v1"
                              : "https://api.example.com/v1"
                          }
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
              )
            }
          </form.Subscribe>
          <div className="flex gap-2">
            <Button type="submit" disabled={busy}>
              {busy ? "Saving…" : provider ? "Save provider" : "Add provider"}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={onCancel}
            >
              Cancel
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function Credentials({
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
  const [editing, setEditing] = useState<ProviderCredential | "new" | null>(
    null,
  );
  const [deleteTarget, setDeleteTarget] = useState<ProviderCredential | null>(
    null,
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const form = useForm({
    defaultValues: {
      name: "",
      weight: "1",
      enabled: true,
      apiKey: "",
      clearKey: false,
    },
    onSubmit: async ({ value }) => {
      if (!editing || busy) return;
      const parsedWeight = Number(value.weight);
      if (!Number.isSafeInteger(parsedWeight) || parsedWeight < 1) {
        setError("Weight must be a positive integer.");
        return;
      }
      setBusy(true);
      setError("");
      const body: CredentialUpdate = {
        name: value.name,
        weight: parsedWeight,
        enabled: value.enabled,
      };
      if (
        kind?.api_key &&
        (editing === "new" || value.clearKey || value.apiKey)
      ) {
        body.api_key = value.clearKey ? "" : value.apiKey;
      }
      try {
        await saveCredential(
          provider.id,
          editing === "new" ? undefined : editing.id,
          body,
        );
        form.reset();
        setEditing(null);
        await queryClient.invalidateQueries({ queryKey: key });
        if (editing !== "new") {
          await queryClient.invalidateQueries({
            queryKey: credentialUsageOptions(provider.id, editing.id).queryKey,
          });
        }
      } catch (err) {
        setError(message(err));
      } finally {
        setBusy(false);
      }
    },
  });

  function edit(credential: ProviderCredential | "new") {
    setError("");
    form.setFieldValue("name", credential === "new" ? "" : credential.name);
    form.setFieldValue(
      "weight",
      credential === "new" ? "1" : String(credential.weight),
    );
    form.setFieldValue(
      "enabled",
      credential === "new" ? true : credential.enabled,
    );
    form.setFieldValue("apiKey", "");
    form.setFieldValue("clearKey", false);
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
    <section className="mt-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="font-medium">Credentials</h3>
        <Button
          variant="outline"
          disabled={!kind || loginBusy}
          onClick={() => edit("new")}
        >
          Add credential
        </Button>
      </div>
      {credentials.isPending && (
        <div className="mt-3">
          <RowsSkeleton label="Loading credentials" />
        </div>
      )}
      {credentials.error && (
        <Alert variant="destructive" className="mt-2">
          <AlertDescription>{message(credentials.error)}</AlertDescription>
        </Alert>
      )}
      {credentials.data?.credentials.length === 0 && (
        <p className="mt-2 text-sm text-muted-foreground">
          No credentials configured.
        </p>
      )}
      <div className="mt-2 grid gap-2">
        {credentials.data?.credentials.map((credential) => (
          <div
            key={credential.id}
            className="flex flex-wrap justify-between gap-4 rounded-lg border p-3"
          >
            <div className="min-w-0">
              <p className="break-words font-medium">{credential.name}</p>
              <p className="mt-1 text-sm text-muted-foreground">
                Weight: {credential.weight} ·{" "}
                {credential.enabled ? "Enabled" : "Disabled"} ·{" "}
                {credential.connected ? "Connected" : "Not connected"}
              </p>
              <div className="mt-2 flex flex-wrap gap-2">
                <Button
                  variant="outline"
                  disabled={loginBusy}
                  onClick={() => edit(credential)}
                >
                  Edit
                </Button>
                <Button
                  variant="outline"
                  disabled={loginBusy}
                  onClick={() => {
                    setError("");
                    setDeleteTarget(credential);
                  }}
                >
                  Delete
                </Button>
                {kind?.oauth &&
                  (provider.kind === "agy" || provider.kind === "codex") && (
                    <Button
                      disabled={loginBusy || !credential.enabled}
                      onClick={() => onLogin(credential)}
                    >
                      {credential.connected ? "Reconnect" : "Sign in"}{" "}
                      {provider.kind === "agy" ? "Google" : "Codex"}
                    </Button>
                  )}
              </div>
            </div>
            {["codex", "agy", "go", "zen"].includes(provider.kind) && (
              <CredentialUsage
                providerId={provider.id}
                credentialId={credential.id}
                credentialName={credential.name}
                kind={provider.kind}
                connected={credential.connected}
              />
            )}
          </div>
        ))}
      </div>
      {editing && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open && !busy) {
              setEditing(null);
              form.reset();
            }
          }}
        >
          <DialogContent
            className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
            showCloseButton={!busy}
          >
            <form
              onSubmit={(event) => {
                event.preventDefault();
                void form.handleSubmit();
              }}
              className="grid gap-5"
            >
              <DialogHeader>
                <DialogTitle>
                  {editing === "new"
                    ? "Add credential"
                    : `Edit ${editing.name}`}
                </DialogTitle>
                <DialogDescription>
                  Configure a named credential for {provider.name}.
                </DialogDescription>
              </DialogHeader>
              {error && (
                <Alert variant="destructive">
                  <AlertDescription>{error}</AlertDescription>
                </Alert>
              )}
              <form.Field name="name">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Name
                    <Input
                      className={inputClass}
                      required
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) =>
                        field.handleChange(event.target.value)
                      }
                    />
                  </Label>
                )}
              </form.Field>
              <form.Field name="weight">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Weight
                    <Input
                      className={inputClass}
                      type="number"
                      required
                      min={1}
                      step={1}
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) =>
                        field.handleChange(event.target.value)
                      }
                    />
                  </Label>
                )}
              </form.Field>
              <form.Field name="enabled">
                {(field) => (
                  <Label className="text-sm">
                    <Checkbox
                      checked={field.state.value}
                      onCheckedChange={(checked) =>
                        field.handleChange(checked === true)
                      }
                    />{" "}
                    Enabled
                  </Label>
                )}
              </form.Field>
              {kind?.api_key && (
                <>
                  <form.Subscribe selector={(state) => state.values.clearKey}>
                    {(clearKey) => (
                      <form.Field name="apiKey">
                        {(field) => (
                          <Label className="block text-sm font-medium">
                            API key{" "}
                            <span className="font-normal text-muted-foreground">
                              (
                              {editing === "new"
                                ? provider.kind === "go"
                                  ? "required"
                                  : "optional; leave blank for a keyless upstream"
                                : "leave blank to retain"}
                              )
                            </span>
                            <Input
                              className={inputClass}
                              type="password"
                              required={
                                provider.kind === "go" && editing === "new"
                              }
                              autoComplete="new-password"
                              disabled={clearKey}
                              value={field.state.value}
                              onBlur={field.handleBlur}
                              onChange={(event) =>
                                field.handleChange(event.target.value)
                              }
                            />
                          </Label>
                        )}
                      </form.Field>
                    )}
                  </form.Subscribe>
                  {editing !== "new" && provider.kind !== "go" && (
                    <form.Field name="clearKey">
                      {(field) => (
                        <Label className="text-sm">
                          <Checkbox
                            checked={field.state.value}
                            onCheckedChange={(checked) =>
                              field.handleChange(checked === true)
                            }
                          />{" "}
                          Use a keyless upstream
                        </Label>
                      )}
                    </form.Field>
                  )}
                </>
              )}
              <div className="flex gap-2">
                <Button type="submit" disabled={busy}>
                  {busy ? "Saving…" : "Save credential"}
                </Button>
                <Button
                  type="button"
                  variant="outline"
                  disabled={busy}
                  onClick={() => {
                    setEditing(null);
                    form.reset();
                  }}
                >
                  Cancel
                </Button>
              </div>
            </form>
          </DialogContent>
        </Dialog>
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

function ProviderCard({
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
  const queryClient = useQueryClient();
  const [google, setGoogle] = useState<GoogleFlow | null>(null);
  const [device, setDevice] = useState<DeviceFlow | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [loginCredential, setLoginCredential] =
    useState<ProviderCredential | null>(null);
  const credentialId = loginCredential?.id;
  const refresh = useCallback(() => {
    void queryClient
      .invalidateQueries({
        queryKey: credentialsOptions(provider.id).queryKey,
      })
      .then(() =>
        queryClient.invalidateQueries({
          queryKey: credentialUsageKey(provider.id),
        }),
      );
  }, [queryClient, provider.id]);
  const callbackForm = useForm({
    defaultValues: { callback: "" },
    onSubmit: async ({ value }) => {
      setBusy(true);
      setError("");
      try {
        await finishGoogleSignIn(provider.id, credentialId, value.callback);
        setGoogle(null);
        callbackForm.setFieldValue("callback", "");
        refresh();
        setNotice("Sign-in complete.");
      } catch (err) {
        setError(message(err));
      } finally {
        setBusy(false);
      }
    },
  });

  useEffect(() => {
    const flow = google ?? device;
    if (!flow || busy) return;
    const controller = new AbortController();
    const kind = google ? "agy" : "codex";
    const delay = google ? 2000 : Math.max(1, device!.interval) * 1000;
    let timer: number;
    const poll = async () => {
      try {
        const result = await pollSignIn(
          provider.id,
          credentialId,
          kind,
          flow.id,
          controller.signal,
        );
        if (controller.signal.aborted) return;
        if (result.status === "complete") {
          setGoogle(null);
          setDevice(null);
          callbackForm.setFieldValue("callback", "");
          refresh();
          setNotice("Sign-in complete.");
        } else if (result.status === "pending") {
          timer = window.setTimeout(() => void poll(), delay);
        } else {
          setGoogle(null);
          setDevice(null);
          setError(result.error || "Sign-in failed.");
        }
      } catch (err) {
        if (!controller.signal.aborted) {
          setGoogle(null);
          setDevice(null);
          setError(message(err));
        }
      }
    };
    timer = window.setTimeout(() => void poll(), delay);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [google, device, busy, provider.id, credentialId, refresh, callbackForm]);

  async function start(credential: ProviderCredential) {
    setLoginCredential(credential);
    setBusy(true);
    setError("");
    setNotice("");
    setGoogle(null);
    setDevice(null);
    callbackForm.setFieldValue("callback", "");
    try {
      if (provider.kind === "agy") {
        const result = await startGoogleSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (url.protocol !== "https:" || url.hostname !== "accounts.google.com")
          throw new Error("Invalid Google authorization URL.");
        setGoogle({ ...result, url: url.href });
      } else if (provider.kind === "codex") {
        const result = await startCodexSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (url.protocol !== "https:" || url.hostname !== "auth.openai.com")
          throw new Error("Invalid Codex authorization URL.");
        setDevice({ ...result, url: url.href });
      }
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="mt-6 p-5">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h2 className="break-words text-lg font-semibold">{provider.name}</h2>
          <p className="mt-1 break-all text-sm text-muted-foreground">
            {kind?.label ?? provider.kind} · {provider.id}
          </p>
        </div>
        <div className="flex shrink-0 gap-2">
          {kind && (kind.api_key || kind.fixed_name === null) && (
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
              deleting ? `Deleting ${provider.name}` : `Delete ${provider.name}`
            }
            disabled={busy || deleting}
            onClick={onDelete}
          >
            <Trash2 />
          </Button>
        </div>
      </div>
      <Credentials
        provider={provider}
        kind={kind}
        onLogin={(credential) => void start(credential)}
        loginBusy={busy || !!google || !!device}
      />
      {kind?.api_key && (
        <p className="mt-2 break-all text-sm text-muted-foreground">
          Upstream: {provider.base_url}
        </p>
      )}
      {provider.kind === "agy" && (
        <p className="mt-3 text-sm text-muted-foreground">
          Configure your Google OAuth client in Settings before signing in.
        </p>
      )}
      {error && (
        <Alert variant="destructive" className="mt-4">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      {notice && (
        <p role="status" className="mt-4 text-sm">
          {notice}
        </p>
      )}
      {(google || device) && (
        <p className="mt-4 text-sm">Signing in: {loginCredential?.name}</p>
      )}
      {google && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void callbackForm.handleSubmit();
          }}
          className="mt-4 grid gap-4"
        >
          <p className="text-sm">
            Open{" "}
            <a
              className="underline"
              href={google.url}
              target="_blank"
              rel="noopener noreferrer"
            >
              Google authorization
            </a>{" "}
            and sign in.{" "}
            {google.local_callback
              ? "Waiting for the local callback. If the callback is on another machine, copy its URL below instead."
              : "Copy the entire redirected URL from the browser address bar; the loopback page may not load."}
          </p>
          <callbackForm.Field name="callback">
            {(field) => (
              <Label className="block text-sm font-medium">
                Redirected callback URL
                <Input
                  className={inputClass}
                  required
                  type="url"
                  autoComplete="off"
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
                />
              </Label>
            )}
          </callbackForm.Field>
          <p className="text-xs text-muted-foreground">
            The callback URL contains a temporary authorization code. Do not
            share it.
          </p>
          <div className="flex gap-2">
            <Button type="submit" disabled={busy}>
              Finish Google sign-in
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => {
                setGoogle(null);
                callbackForm.setFieldValue("callback", "");
              }}
            >
              Cancel
            </Button>
          </div>
        </form>
      )}
      {device && (
        <div className="mt-4 grid gap-3 text-sm">
          <p>
            Open{" "}
            <a
              className="underline"
              href={device.url}
              target="_blank"
              rel="noopener noreferrer"
            >
              OpenAI device authorization
            </a>{" "}
            and enter this code:
          </p>
          <code className="select-all text-lg font-semibold">
            {device.user_code}
          </code>
          <p role="status" className="text-muted-foreground">
            Waiting for authorization…
          </p>
          <div>
            <Button variant="outline" onClick={() => setDevice(null)}>
              Cancel
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}

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
                Delete provider “{deleteTarget.name}” ({deleteTarget.id}) and
                its credentials? This cannot be undone, and the ID cannot be
                reused.
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
