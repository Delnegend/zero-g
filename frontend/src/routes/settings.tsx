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

const inputClass =
  "mt-1 h-9 w-full rounded-lg border border-input bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";

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
      log_retention_days: settings?.log_retention_days ?? 1,
    },
    onSubmit: ({ value }) => save(value),
  });

  useEffect(() => {
    if (settings && !draftStarted.current) {
      form.setFieldValue("log_retention_days", settings.log_retention_days);
    }
  }, [settings, form]);

  async function save(value: { log_retention_days: number }) {
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
      draftStarted.current = logRetention !== updated.log_retention_days;
      queryClient.setQueryData(settingsOptions().queryKey, updated);
      form.setFieldValue("log_retention_days", logRetention);
      setSaved(true);
    } catch (err) {
      if (!controller.signal.aborted) setError(message(err));
    } finally {
      saveMutation.reset();
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return (
    <main className="mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
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
        <Card className="mt-6 p-5">
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void form.handleSubmit();
            }}
            className="grid gap-5"
          >
            <form.Field name="log_retention_days">
              {(field) => (
                <Label className="block text-sm font-medium">
                  Log retention (days)
                  <Input
                    className={inputClass}
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
            <div>
              <Button type="submit" disabled={busy}>
                {busy ? "Saving…" : "Save settings"}
              </Button>
            </div>
          </form>
        </Card>
      )}
    </main>
  );
}
