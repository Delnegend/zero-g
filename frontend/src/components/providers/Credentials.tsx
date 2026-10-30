import { CredentialUsage } from "@/components/CredentialUsage";
import { RowsSkeleton } from "@/components/Loading";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { message } from "@/server/api/client";
import {
  credentialsOptions,
  credentialUsageOptions,
  deleteCredential,
  saveCredential,
  type CredentialUpdate,
  type Provider,
  type ProviderCredential,
  type ProviderKind,
} from "@/server/api/providers";
import { useForm } from "@tanstack/react-form";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Pencil, Trash2 } from "lucide-react";
import { useState } from "react";
import { CredentialConfiguration } from "./CredentialConfiguration";
import { inputClass } from "./inputClass";

export function Credentials({
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
      <div className="mt-2 grid grid-cols-[minmax(0,1fr)] gap-2">
        {credentials.data?.credentials.map((credential) => (
          <div
            key={credential.id}
            className="flex flex-wrap justify-between gap-4 rounded-lg border p-3"
          >
            <div
              className={
                provider.kind === "zen" || provider.kind === "openrouter"
                  ? "min-w-0 flex-[1_1_20rem]"
                  : "min-w-0"
              }
            >
              <p className="break-words font-medium">
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
                    • <span className="capitalize">{credential.plan_type}</span>
                  </span>
                )}
              </p>
              <p className="mt-1 text-sm text-muted-foreground">
                Weight: {credential.weight} ·{" "}
                {credential.enabled ? "Enabled" : "Disabled"} ·{" "}
                {credential.connected ? "Connected" : "Not connected"}
              </p>
              <div
                className={
                  provider.kind === "zen" || provider.kind === "openrouter"
                    ? "mt-2 flex flex-wrap gap-1.5"
                    : "mt-2 flex flex-wrap gap-2"
                }
              >
                <Button
                  variant="outline"
                  size="icon"
                  aria-label={`Edit ${credential.name}`}
                  disabled={loginBusy}
                  onClick={() => edit(credential)}
                >
                  <Pencil />
                </Button>
                <Button
                  variant="outline"
                  size="icon"
                  aria-label={`Delete ${credential.name}`}
                  disabled={loginBusy}
                  onClick={() => {
                    setError("");
                    setDeleteTarget(credential);
                  }}
                >
                  <Trash2 />
                </Button>
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
            {["codex", "agy", "claude", "go", "zen", "openrouter"].includes(
              provider.kind,
            ) && (
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
