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

/** Every editable setting; all are whole numbers. */
const FIELDS = [
  "log_retention_days",
  "body_retention_days",
  "pricing_interval_hours",
  "discovery_interval_minutes",
] as const;
type Field = (typeof FIELDS)[number];

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
      log_retention_days: settings?.log_retention_days ?? 90,
      body_retention_days: settings?.body_retention_days ?? 7,
      pricing_interval_hours: settings?.pricing_interval_hours ?? 24,
      discovery_interval_minutes: settings?.discovery_interval_minutes ?? 60,
    },
    onSubmit: ({ value }) => save(value),
  });

  useEffect(() => {
    if (settings && !draftStarted.current) {
      for (const name of FIELDS) form.setFieldValue(name, settings[name]);
    }
  }, [settings, form]);

  async function save(value: SettingsUpdate & Record<Field, number>) {
    if (!settings) return;
    saveController.current?.abort();
    const controller = new AbortController();
    saveController.current = controller;
    setBusy(true);
    setError("");
    setSaved(false);
    try {
      const updated = await saveMutation.mutateAsync({
        body: value,
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      await queryClient.cancelQueries({ queryKey: settingsOptions().queryKey });
      if (controller.signal.aborted) return;
      // Keep edits made while the save was in flight; take the server value otherwise.
      const current = form.state.values;
      draftStarted.current = false;
      for (const name of FIELDS) {
        const next =
          current[name] === value[name] ? updated[name] : current[name];
        if (next !== updated[name]) draftStarted.current = true;
        form.setFieldValue(name, next);
      }
      queryClient.setQueryData(settingsOptions().queryKey, updated);
      setSaved(true);
    } catch (err) {
      if (!controller.signal.aborted) setError(message(err));
    } finally {
      saveMutation.reset();
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  const dayField = (
    name: "log_retention_days" | "body_retention_days",
    label: string,
  ) => (
    <form.Field name={name}>
      {(field) => (
        <Label className="grid gap-2">
          {label}
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
  );

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
                Request bodies (the full prompt and response) take most of the
                space, so they expire sooner than the rest of the log. Expired
                records are removed at startup and then hourly.
              </CardDescription>
            </CardHeader>
            <CardContent className="grid gap-4 sm:grid-cols-2">
              {dayField("log_retention_days", "Logs, charts and totals (days)")}
              {dayField("body_retention_days", "Prompts and responses (days)")}
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
