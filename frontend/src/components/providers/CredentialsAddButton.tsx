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
  saveCredential,
  type CredentialUpdate,
  type Provider,
  type ProviderCredential,
  type ProviderKind,
} from "@/server/api/providers";
import { useForm } from "@tanstack/react-form";
import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

export function CredentialsAddButton({
  provider,
  kind,
  editing,
  onClose,
}: {
  provider: Provider;
  kind?: ProviderKind;
  editing: ProviderCredential | "new";
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const key = credentialsOptions(provider.id).queryKey;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const form = useForm({
    defaultValues: {
      name: editing === "new" ? "" : editing.name,
      weight: editing === "new" ? "1" : String(editing.weight),
      enabled: editing === "new" ? true : editing.enabled,
      apiKey: "",
    },
    onSubmit: async ({ value }) => {
      if (busy) return;
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
      if (kind?.api_key && (editing === "new" || value.apiKey)) {
        body.api_key = value.apiKey;
      }
      try {
        await saveCredential(
          provider.id,
          editing === "new" ? undefined : editing.id,
          body,
        );
        onClose();
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

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) {
          onClose();
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
              {editing === "new" ? "Add credential" : `Edit ${editing.name}`}
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
                  className="mt-1"
                  required
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
                />
              </Label>
            )}
          </form.Field>
          <form.Field name="weight">
            {(field) => (
              <Label className="block text-sm font-medium">
                Weight
                <Input
                  className="mt-1"
                  type="number"
                  required
                  min={1}
                  step={1}
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
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
            <form.Field name="apiKey">
              {(field) => (
                <Label className="block text-sm font-medium">
                  API key{" "}
                  <span className="font-normal text-muted-foreground">
                    (
                    {editing === "new"
                      ? provider.kind === "go" ||
                        provider.kind === "commandcode" ||
                        provider.kind === "commandcode-go"
                        ? "required"
                        : "optional"
                      : "leave blank to retain"}
                    )
                  </span>
                  <Input
                    className="mt-1"
                    type="password"
                    required={
                      (provider.kind === "go" ||
                        provider.kind === "commandcode" ||
                        provider.kind === "commandcode-go") &&
                      editing === "new"
                    }
                    value={field.state.value}
                    onBlur={field.handleBlur}
                    onChange={(event) => field.handleChange(event.target.value)}
                  />
                </Label>
              )}
            </form.Field>
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
                onClose();
              }}
            >
              Cancel
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
