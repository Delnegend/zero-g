import { useForm } from "@tanstack/react-form";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
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
  const [validationError, setValidationError] = useState("");
  const queryClient = useQueryClient();
  const save = useMutation({
    gcTime: 0,
    mutationFn: (body: Record<string, unknown>) =>
      saveProvider(provider?.id, body),
    onSuccess: async (updated) => {
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
    },
  });
  const busy = save.isPending;
  const error = validationError || (save.error ? message(save.error) : "");
  const form = useForm({
    defaultValues: {
      id: provider?.id ?? "",
      name: provider?.name ?? "",
      kind: provider?.kind ?? kinds[0]?.value ?? "",
      baseUrl: provider?.base_url ?? "",
      keyless: provider?.keyless ?? false,
    },
    onSubmit: ({ value }) => {
      if (busy) return;
      setValidationError("");
      save.reset();
      const kind = kinds.find((item) => item.value === value.kind);
      if (!kind) {
        setValidationError("Provider kind is unavailable.");
        return;
      }
      const body: Record<string, unknown> = {};
      if (kind.fixed_name === null) {
        body.name = value.name;
        body.keyless = value.keyless;
      }
      if (
        kind.api_key &&
        value.kind !== "zen" &&
        value.kind !== "openrouter" &&
        value.kind !== "chatjimmy" &&
        value.kind !== "commandcode" &&
        value.kind !== "commandcode-go"
      ) {
        body.base_url = value.baseUrl;
      }
      if (!provider) {
        if (kind.fixed_id === null) body.id = value.id;
        body.kind = value.kind;
      }
      save.mutate(body);
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
                            className="mt-1"
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
                            className="mt-1"
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
                  {metadata?.fixed_name === null && (
                    <form.Field name="keyless">
                      {(field) => (
                        <Label className="text-sm">
                          <Checkbox
                            checked={field.state.value}
                            onCheckedChange={(checked) =>
                              field.handleChange(checked === true)
                            }
                          />{" "}
                          Keyless upstream (no API key required)
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
                  items={kinds.map(({ value, label, fixed_name }) => ({
                    value,
                    label: label ?? fixed_name ?? value,
                  }))}
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
                    {kinds.map(({ value, label, fixed_name }) => (
                      <SelectItem key={value} value={value}>
                        {label ?? fixed_name ?? value}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </form.Field>
          </div>
          <form.Subscribe selector={(state) => state.values.kind}>
            {(kind) =>
              kind === "zen" ||
              kind === "openrouter" ||
              kind === "chatjimmy" ||
              kind === "commandcode" ||
              kind === "commandcode-go" ? (
                <p className="text-sm text-muted-foreground">
                  {kind === "zen"
                    ? "Zen uses the official OpenCode API for all models."
                    : kind === "openrouter"
                      ? "OpenRouter uses the official OpenRouter API for all models."
                      : kind === "commandcode"
                        ? "Command Code uses the official Command Code Provider API. Claude models are not listed because they need Anthropic's message format."
                        : kind === "commandcode-go"
                          ? "Command Code Go uses the Command Code CLI's private endpoint, so the $1 Go plan works. It lists only Go plan models and may break when Command Code changes the CLI."
                          : "ChatJimmy uses the official chatjimmy.ai inference endpoint for all models."}
                </p>
              ) : (
                kinds.find((item) => item.value === kind)?.api_key && (
                  <form.Field name="baseUrl">
                    {(field) => (
                      <Label className="block text-sm font-medium">
                        Upstream base URL
                        <Input
                          className="mt-1"
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
