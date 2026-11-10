import { RowsSkeleton } from "@/components/Loading";
import { FilterGroup } from "@/components/ObservabilityFilters";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Calendar } from "@/components/ui/calendar";
import { Card } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { toast, Toaster } from "@/components/ui/toast";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { formatDate, formatDateYMD } from "@/lib/date";
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
import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Ban, CalendarDays, Clock, Pencil } from "lucide-react";
import { useEffect, useState } from "react";

export const Route = createFileRoute("/keys")({
  head: () => ({ meta: [{ title: "Virtual keys · Zero-G" }] }),
  component: KeysPage,
});

function localDateTime(seconds: number | null | undefined) {
  if (seconds == null) return "";
  const date = new Date(seconds * 1000);
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 19);
}

function expiration(value: string): number | null {
  if (!value) return null;
  if (
    !/^\d{4}-\d{2}-\d{2}T(?:[01]\d|2[0-3]):[0-5]\d(?::[0-5]\d)?$/.test(value)
  ) {
    throw new Error("Enter a valid time as HH:MM or HH:MM:SS.");
  }
  const seconds = Math.floor(new Date(value).getTime() / 1000);
  if (!Number.isFinite(seconds) || seconds <= Date.now() / 1000) {
    throw new Error("Expiration must be in the future.");
  }
  return seconds;
}

function ExpirationStatus({
  expiresAt,
}: {
  expiresAt: number | null | undefined;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (expiresAt == null) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [expiresAt]);
  if (expiresAt == null) return <span>Never expires</span>;
  const date = new Date(expiresAt * 1000);
  const minutes = Math.ceil((date.getTime() - now) / 60_000);
  const relative = [
    Math.floor(minutes / 1440) > 0 && `${Math.floor(minutes / 1440)}d`,
    Math.floor((minutes % 1440) / 60) > 0 &&
      `${Math.floor((minutes % 1440) / 60)}h`,
    minutes % 60 > 0 && `${minutes % 60}m`,
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <Tooltip>
      <TooltipTrigger className="text-left whitespace-nowrap text-muted-foreground underline decoration-dotted underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2">
        {minutes > 0 ? `Expires in ${relative}` : "Expired"}
      </TooltipTrigger>
      <TooltipContent>{date.toLocaleString()}</TooltipContent>
    </Tooltip>
  );
}

function ExpirationField({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const datePart = value.split("T")[0];
  const date = datePart ? new Date(`${datePart}T12:00:00`) : undefined;
  const selected = date && Number.isFinite(date.getTime()) ? date : undefined;
  const time = value.includes("T") ? value.split("T")[1] : "";
  return (
    <div className="grid gap-2">
      <span className="text-sm font-medium">Expiration (optional)</span>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          render={
            <Button
              type="button"
              variant="outline"
              disabled={disabled}
              aria-label={`Choose expiration date: ${selected ? `${formatDate(selected)} ${time}` : "never expires"}`}
              className="w-full justify-start text-left font-normal"
            />
          }
        >
          <CalendarDays />
          <span className="min-w-0 truncate">
            {selected ? `${formatDate(selected)} · ${time}` : "Never expires"}
          </span>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="w-62.5 max-w-[calc(100vw-1rem)] p-0"
        >
          <Calendar
            classNames={{ root: "rdp-root w-full" }}
            mode="single"
            selected={selected}
            defaultMonth={selected}
            onSelect={(day) => {
              if (day) onChange(`${formatDateYMD(day)}T${time || "23:59:59"}`);
            }}
          />
          <Label className="mx-3 flex items-center justify-between gap-3 border-t pt-3 text-sm">
            Time (local)
            <div className="relative flex items-center">
              <Input
                className="h-8 w-32 pr-7 font-mono text-xs"
                type="time"
                step="1"
                aria-label="Time (24-hour)"
                placeholder="HH:MM:SS"
                disabled={disabled || !selected}
                value={time}
                onChange={(event) =>
                  onChange(`${datePart}T${event.target.value}`)
                }
              />
              <Clock
                aria-hidden="true"
                className="pointer-events-none absolute right-2 size-3.5 text-muted-foreground"
              />
            </div>
          </Label>
          <div className="flex justify-between gap-2 px-3 pb-3">
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={disabled || !selected}
              onClick={() => {
                onChange("");
                setOpen(false);
              }}
            >
              Clear expiration
            </Button>
            <Button type="button" size="sm" onClick={() => setOpen(false)}>
              Done
            </Button>
          </div>
        </PopoverContent>
      </Popover>
    </div>
  );
}

function KeyForm({
  defaultValues,
  submitLabel,
  busy,
  optionsUnavailable,
  providers,
  catalogProviders,
  onCancel,
  onSubmit,
}: {
  defaultValues: {
    name: string;
    providers: string[];
    models: string[];
    expiresAt: string;
  };
  submitLabel: string;
  busy: boolean;
  optionsUnavailable: boolean;
  providers: Array<{ id: string; name: string }>;
  catalogProviders: Array<{
    id: string;
    name: string;
    models: Array<{ model: { id: string } }>;
  }>;
  onCancel: () => void;
  onSubmit: (value: {
    name: string;
    providers: string[];
    models: string[];
    expiresAt: string;
  }) => void;
}) {
  const form = useForm({
    defaultValues,
    onSubmit: ({ value }) => onSubmit(value),
  });

  return (
    <form
      className="grid gap-5"
      onSubmit={(event) => {
        event.preventDefault();
        void form.handleSubmit();
      }}
    >
      <form.Field name="name">
        {(field) => (
          <Label className="block text-sm font-medium">
            Name
            <Input
              className="mt-1"
              disabled={busy}
              required
              value={field.state.value}
              onBlur={field.handleBlur}
              onChange={(event) => field.handleChange(event.target.value)}
            />
          </Label>
        )}
      </form.Field>
      <div>
        <p className="text-xs text-muted-foreground">
          Select none to allow all providers.
        </p>
        <form.Field name="providers">
          {(field) => (
            <FilterGroup
              title="Providers"
              values={providers.map((p) => p.id)}
              identities={providers}
              selected={field.state.value}
              onChange={(nextProviders) => {
                field.handleChange(nextProviders);
                if (nextProviders.length > 0) {
                  const validModels = new Set(
                    catalogProviders
                      .filter((p) => nextProviders.includes(p.id))
                      .flatMap((p) => p.models.map((m) => m.model.id)),
                  );
                  form.setFieldValue("models", (prev) =>
                    prev.filter((id) => validModels.has(id)),
                  );
                }
              }}
              defaultOpen
              disabled={busy}
            />
          )}
        </form.Field>
      </div>
      <div>
        <p className="text-xs text-muted-foreground">
          Select none to allow all models. Model IDs are upstream IDs.
        </p>
        <form.Subscribe selector={(state) => state.values.providers}>
          {(selectedProviders) => {
            const availableModels = [
              ...new Set(
                catalogProviders
                  .filter(
                    (provider) =>
                      selectedProviders.length === 0 ||
                      selectedProviders.includes(provider.id),
                  )
                  .flatMap((provider) =>
                    provider.models.map((model) => model.model.id),
                  ),
              ),
            ];
            return (
              <form.Field name="models">
                {(field) => (
                  <FilterGroup
                    title="Models"
                    values={availableModels}
                    selected={field.state.value}
                    onChange={field.handleChange}
                    defaultOpen
                    disabled={busy}
                  />
                )}
              </form.Field>
            );
          }}
        </form.Subscribe>
      </div>
      {!catalogProviders.some((provider) => provider.models.length > 0) && (
        <p className="text-sm text-muted-foreground">
          No models found in the catalog. Selecting no models allows all models.
        </p>
      )}
      <form.Field name="expiresAt">
        {(field) => (
          <ExpirationField
            value={field.state.value}
            onChange={field.handleChange}
            disabled={busy}
          />
        )}
      </form.Field>
      <div className="flex flex-wrap gap-2">
        <Button
          type="button"
          variant="outline"
          disabled={busy}
          onClick={onCancel}
        >
          Cancel
        </Button>
        <Button type="submit" disabled={busy || optionsUnavailable}>
          {busy ? "Working…" : submitLabel}
        </Button>
      </div>
    </form>
  );
}

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
    <main className="mx-auto w-full max-w-7xl min-w-0 px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Virtual keys</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Create keys for inference access and restrict their providers and
        models.
      </p>
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
              : message(keysQuery.error)}
            <Button
              variant="outline"
              className="ml-3"
              onClick={() => void refreshKeys()}
            >
              Retry key list
            </Button>
          </AlertDescription>
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
          {newKey && <p role="status">{success}</p>}
          {refreshError && (
            <Alert variant="destructive">
              <AlertDescription>
                The key list could not be refreshed and may be out of date.{" "}
                {refreshError}
                <Button
                  variant="outline"
                  className="ml-3"
                  onClick={() => void refreshKeys()}
                >
                  Retry key list
                </Button>
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
                className="flex flex-wrap items-center justify-between gap-4 py-4"
              >
                <div>
                  <h3 className="font-medium">{key.name}</h3>
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
                <div className="flex flex-wrap gap-2">
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
    </main>
  );
}
