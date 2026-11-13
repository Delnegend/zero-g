import { createFileRoute } from "@tanstack/react-router";
import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { message } from "@/server/api/client";
import { FormSkeleton } from "@/components/Loading";
import { Page } from "@/routes/__root";
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
    <Page
      title="Settings"
      description="Configure the gateway. Some changes may require a restart."
    >
      {displayError && (
        <Alert variant="destructive" className="mb-6">
          <AlertDescription>{displayError}</AlertDescription>
        </Alert>
      )}
      {saved && (
        <p role="status" className="mb-6 text-sm">
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
          className="grid gap-6"
        >
          <Card className="gap-6 [--card-spacing:--spacing(6)]">
            <CardHeader className="gap-2">
              <CardTitle className="text-lg font-semibold">
                <h2>Log retention</h2>
              </CardTitle>
              <CardDescription>
                Configure how long request logs and traces are retained. Expired
                records are removed at startup and then hourly.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <form.Field name="log_retention_days">
                {(field) => (
                  <Label className="grid gap-2">
                    Log retention (days)
                    <Input
                      className="h-10"
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
            </CardContent>
          </Card>

          <Card className="gap-6 [--card-spacing:--spacing(6)]">
            <CardHeader className="gap-2">
              <CardTitle className="text-lg font-semibold">
                <h2>Model synchronization</h2>
              </CardTitle>
              <CardDescription>
                Configure periodic background intervals for refreshing pricing
                datasheets and discovering provider models. A value of 0
                disables periodic refreshes.
              </CardDescription>
            </CardHeader>
            <CardContent className="grid gap-4 sm:grid-cols-2">
              <form.Field name="pricing_interval_hours">
                {(field) => (
                  <Label className="grid gap-2">
                    Datasheet sync interval (hours)
                    <Input
                      className="h-10"
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
                  <Label className="grid gap-2">
                    Model discovery interval (minutes)
                    <Input
                      className="h-10"
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
            </CardContent>
          </Card>

          <div>
            <Button type="submit" disabled={busy}>
              {busy ? "Saving…" : "Save settings"}
            </Button>
          </div>
        </form>
      )}
    </Page>
  );
}
