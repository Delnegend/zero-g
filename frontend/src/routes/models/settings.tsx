import { createFileRoute } from "@tanstack/react-router";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { message } from "@/server/api/client";
import {
  modelCatalogQueryOptions,
  modelSettingsQueryOptions,
  saveModelSettings,
  syncModelSettings,
  type Settings,
} from "@/server/api/models";
import { FormSkeleton } from "@/components/Loading";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export const Route = createFileRoute("/models/settings")({
  head: () => ({ meta: [{ title: "Model settings · Zero-G" }] }),
  component: ModelSettingsPage,
});

const pageClass =
  "mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12";
const inputClass = "mt-1 w-full";

function SettingsForm({ initial }: { initial: Settings }) {
  const queryClient = useQueryClient();
  const [validationError, setValidationError] = useState("");
  const save = useMutation({
    mutationFn: saveModelSettings,
    onSuccess: async (updated, value) => {
      await queryClient.cancelQueries({
        queryKey: modelSettingsQueryOptions().queryKey,
      });
      if (form.state.values.pricing_url === value.pricing_url) {
        form.setFieldValue("pricing_url", updated.pricing_url);
      }
      if (form.state.values.parameters_url === value.parameters_url) {
        form.setFieldValue("parameters_url", updated.parameters_url);
      }
      queryClient.setQueryData(modelSettingsQueryOptions().queryKey, updated);
    },
  });
  const sync = useMutation({
    mutationFn: syncModelSettings,
    onSuccess: async () => {
      await queryClient.invalidateQueries({
        queryKey: modelCatalogQueryOptions().queryKey,
      });
    },
  });
  const error =
    validationError ||
    (save.error ? message(save.error) : "") ||
    (sync.error ? message(sync.error) : "");
  const status = save.isSuccess
    ? "Settings saved."
    : sync.isSuccess
      ? "Datasheets synced."
      : "";
  function clearStatus() {
    if (save.isSuccess) save.reset();
    if (sync.isSuccess) sync.reset();
  }
  const form = useForm({
    defaultValues: initial,
    onSubmit: ({ value }) => {
      save.reset();
      sync.reset();
      setValidationError("");
      if (
        ![value.pricing_interval_hours, value.discovery_interval_minutes].every(
          (n) => Number.isSafeInteger(n) && n >= 0,
        )
      ) {
        setValidationError("Intervals must be non-negative whole numbers.");
        return;
      }
      save.mutate(value);
    },
  });
  function forceSync() {
    save.reset();
    sync.reset();
    setValidationError("");
    sync.mutate();
  }
  return (
    <>
      {error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      {status && (
        <p role="status" className="mt-5 text-sm">
          {status}
        </p>
      )}
      <Card className="mt-6 p-5">
        <form
          className="grid gap-5"
          onSubmit={(event) => {
            event.preventDefault();
            void form.handleSubmit();
          }}
        >
          <form.Field name="pricing_url">
            {(field) => (
              <Label className="block">
                Pricing datasheet URL
                <Input
                  className={inputClass}
                  type="url"
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => {
                    clearStatus();
                    field.handleChange(event.target.value);
                  }}
                />
              </Label>
            )}
          </form.Field>
          <form.Field name="parameters_url">
            {(field) => (
              <Label className="block">
                Parameters datasheet URL
                <Input
                  className={inputClass}
                  type="url"
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => {
                    clearStatus();
                    field.handleChange(event.target.value);
                  }}
                />
              </Label>
            )}
          </form.Field>
          <p className="text-sm text-muted-foreground">
            Leave a datasheet URL blank to restore the default.
          </p>
          <div className="grid gap-5 sm:grid-cols-2">
            <form.Field name="pricing_interval_hours">
              {(field) => (
                <Label className="block">
                  Datasheet sync interval (hours)
                  <Input
                    className={inputClass}
                    type="number"
                    min="0"
                    step="1"
                    required
                    value={
                      Number.isNaN(field.state.value) ? "" : field.state.value
                    }
                    onBlur={field.handleBlur}
                    onChange={(event) => {
                      clearStatus();
                      field.handleChange(event.target.valueAsNumber);
                    }}
                  />
                </Label>
              )}
            </form.Field>
            <form.Field name="discovery_interval_minutes">
              {(field) => (
                <Label className="block">
                  Model discovery interval (minutes)
                  <Input
                    className={inputClass}
                    type="number"
                    min="0"
                    step="1"
                    required
                    value={
                      Number.isNaN(field.state.value) ? "" : field.state.value
                    }
                    onBlur={field.handleBlur}
                    onChange={(event) => {
                      clearStatus();
                      field.handleChange(event.target.valueAsNumber);
                    }}
                  />
                </Label>
              )}
            </form.Field>
          </div>
          <p className="text-sm text-muted-foreground">
            Set an interval to 0 to disable its automatic updates. Manual
            refresh and sync remain available.
          </p>
          <div className="flex flex-wrap gap-3">
            <Button type="submit" disabled={save.isPending || sync.isPending}>
              {save.isPending ? "Saving…" : "Save settings"}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={save.isPending || sync.isPending}
              onClick={() => void forceSync()}
            >
              {sync.isPending ? "Syncing…" : "Sync datasheets now"}
            </Button>
          </div>
          <p className="text-sm text-muted-foreground">
            Sync uses saved settings. Save changes before syncing.
          </p>
        </form>
      </Card>
    </>
  );
}

export function ModelSettingsPage() {
  const settings = useQuery(modelSettingsQueryOptions());
  return (
    <main className={pageClass}>
      <h1 className="text-3xl font-semibold tracking-tight">Model settings</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Configure datasheets and automatic model discovery.
      </p>
      {settings.isLoading && <FormSkeleton modelSettings />}
      {settings.error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{message(settings.error)}</AlertDescription>
        </Alert>
      )}
      {settings.data && <SettingsForm initial={settings.data} />}
    </main>
  );
}
