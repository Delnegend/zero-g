import { useForm } from "@tanstack/react-form";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
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
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { message } from "@/server/api/client";
import {
  providersOptions,
  saveProvider,
  type Provider,
  type ProviderKind,
} from "@/server/api/providers";
import { inputClass } from "./inputClass";

export function ProviderForm({
  provider,
  kinds: availableKinds,
  onDone,
  onCancel,
}: {
  provider?: Provider;
  kinds: ProviderKind[];
  onDone: () => void;
  onCancel: () => void;
}) {
  const [initialKinds] = useState(availableKinds);
  const kinds = provider ? availableKinds : initialKinds;
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
      if (kind.api_key && value.kind !== "zen") body.base_url = value.baseUrl;
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
              kind === "zen" ? (
                <p className="text-sm text-muted-foreground">
                  Zen uses the official OpenCode API for all models.
                </p>
              ) : (
                kinds.find((item) => item.value === kind)?.api_key && (
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
                )
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
