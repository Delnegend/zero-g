import { createFileRoute } from "@tanstack/react-router";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Card } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { message } from "@/server/api/client";
import { FormSkeleton } from "@/components/Loading";
import {
  settingsOptions,
  updateSettings,
  type SettingsUpdate,
} from "@/server/api/settings";

export const Route = createFileRoute("/settings")({
  head: () => ({ meta: [{ title: "Settings · Zero-G" }] }),
  component: SettingsPage,
});

export function SettingsPage() {
  const draftStarted = useRef(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const saveController = useRef<AbortController | null>(null);

  const settingsQuery = useQuery(settingsOptions());
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
      log_retention_days: settings?.log_retention_days ?? 30,
      pricing_interval_hours: settings?.pricing_interval_hours ?? 24,
      discovery_interval_minutes: settings?.discovery_interval_minutes ?? 60,
    },
    onSubmit: ({ value }) => save(value),
  });

  useEffect(() => {
    if (settings && !draftStarted.current) {
      form.setFieldValue("log_retention_days", settings.log_retention_days);
      form.setFieldValue(
        "pricing_interval_hours",
        settings.pricing_interval_hours,
      );
      form.setFieldValue(
        "discovery_interval_minutes",
        settings.discovery_interval_minutes,
      );
    }
  }, [settings, form]);

  async function save(value: {
    log_retention_days: number;
    pricing_interval_hours: number;
    discovery_interval_minutes: number;
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
        pricing_interval_hours: value.pricing_interval_hours,
        discovery_interval_minutes: value.discovery_interval_minutes,
      };
      const updated = await saveMutation.mutateAsync({
        body,
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      await queryClient.cancelQueries({ queryKey: settingsOptions().queryKey });
      if (controller.signal.aborted) return;
      const current = form.state.values;
      const logRetention =
        current.log_retention_days === value.log_retention_days
          ? updated.log_retention_days
          : current.log_retention_days;
      const pricingInterval =
        current.pricing_interval_hours === value.pricing_interval_hours
          ? updated.pricing_interval_hours
          : current.pricing_interval_hours;
      const discoveryInterval =
        current.discovery_interval_minutes === value.discovery_interval_minutes
          ? updated.discovery_interval_minutes
          : current.discovery_interval_minutes;
      draftStarted.current =
        logRetention !== updated.log_retention_days ||
        pricingInterval !== updated.pricing_interval_hours ||
        discoveryInterval !== updated.discovery_interval_minutes;
      queryClient.setQueryData(settingsOptions().queryKey, updated);
      form.setFieldValue("log_retention_days", logRetention);
      form.setFieldValue("pricing_interval_hours", pricingInterval);
      form.setFieldValue("discovery_interval_minutes", discoveryInterval);
      setSaved(true);
    } catch (err) {
      if (!controller.signal.aborted) setError(message(err));
    } finally {
      saveMutation.reset();
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return (
    <main className="mx-auto w-full max-w-7xl min-w-0 px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Settings</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Configure the gateway. Some changes may require a restart.
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
      {settingsQuery.isPending && <FormSkeleton />}
      {settings && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void form.handleSubmit();
          }}
          className="mt-6 grid gap-6"
        >
          <Card className="p-5">
            <h2 className="text-lg font-semibold">Log retention</h2>
            <p className="mt-1 text-sm text-muted-foreground">
              Configure how long request logs and traces are retained before
              being pruned.
            </p>
            <div className="mt-4">
              <form.Field name="log_retention_days">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Log retention (days)
                    <Input
                      className="mt-1"
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
            </div>
          </Card>

          <Card className="p-5">
            <h2 className="text-lg font-semibold">Model synchronization</h2>
            <p className="mt-1 text-sm text-muted-foreground">
              Configure periodic background intervals for refreshing pricing
              datasheets and discovering provider models. A value of 0 disables
              periodic refreshes.
            </p>
            <div className="mt-4 grid gap-4 sm:grid-cols-2">
              <form.Field name="pricing_interval_hours">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Datasheet sync interval (hours)
                    <Input
                      className="mt-1"
                      type="number"
                      min={0}
                      step={1}
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
              <form.Field name="discovery_interval_minutes">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Model discovery interval (minutes)
                    <Input
                      className="mt-1"
                      type="number"
                      min={0}
                      step={1}
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
            </div>
          </Card>

          <div>
            <Button type="submit" disabled={busy}>
              {busy ? "Saving…" : "Save settings"}
            </Button>
          </div>
        </form>
      )}
    </main>
  );
}
