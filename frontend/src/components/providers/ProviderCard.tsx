import { useForm } from "@tanstack/react-form";
import { useQueryClient } from "@tanstack/react-query";
import { Pencil, Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { message } from "@/server/api/client";
import {
  credentialsOptions,
  credentialUsageKey,
  finishGoogleSignIn,
  pollSignIn,
  startCodexSignIn,
  startGoogleSignIn,
  type DeviceFlow,
  type GoogleFlow,
  type Provider,
  type ProviderCredential,
  type ProviderKind,
} from "@/server/api/providers";
import { Credentials } from "./Credentials";
import { inputClass } from "./inputClass";

export function ProviderCard({
  provider,
  kind,
  onEdit,
  onDelete,
  deleting,
}: {
  provider: Provider;
  kind?: ProviderKind;
  onEdit: () => void;
  onDelete: () => void;
  deleting: boolean;
}) {
  const queryClient = useQueryClient();
  const [google, setGoogle] = useState<GoogleFlow | null>(null);
  const [device, setDevice] = useState<DeviceFlow | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [loginCredential, setLoginCredential] =
    useState<ProviderCredential | null>(null);
  const credentialId = loginCredential?.id;
  const refresh = useCallback(() => {
    void queryClient
      .invalidateQueries({
        queryKey: credentialsOptions(provider.id).queryKey,
      })
      .then(() =>
        queryClient.invalidateQueries({
          queryKey: credentialUsageKey(provider.id),
        }),
      );
  }, [queryClient, provider.id]);
  const callbackForm = useForm({
    defaultValues: { callback: "" },
    onSubmit: async ({ value }) => {
      setBusy(true);
      setError("");
      try {
        await finishGoogleSignIn(provider.id, credentialId, value.callback);
        setGoogle(null);
        callbackForm.setFieldValue("callback", "");
        refresh();
        setNotice("Sign-in complete.");
      } catch (err) {
        setError(message(err));
      } finally {
        setBusy(false);
      }
    },
  });

  useEffect(() => {
    const flow = google ?? device;
    if (!flow || busy) return;
    const controller = new AbortController();
    const kind = google ? "agy" : "codex";
    const delay = google ? 2000 : Math.max(1, device!.interval) * 1000;
    let timer: number;
    const poll = async () => {
      try {
        const result = await pollSignIn(
          provider.id,
          credentialId,
          kind,
          flow.id,
          controller.signal,
        );
        if (controller.signal.aborted) return;
        if (result.status === "complete") {
          setGoogle(null);
          setDevice(null);
          callbackForm.setFieldValue("callback", "");
          refresh();
          setNotice("Sign-in complete.");
        } else if (result.status === "pending") {
          timer = window.setTimeout(() => void poll(), delay);
        } else {
          setGoogle(null);
          setDevice(null);
          setError(result.error || "Sign-in failed.");
        }
      } catch (err) {
        if (!controller.signal.aborted) {
          setGoogle(null);
          setDevice(null);
          setError(message(err));
        }
      }
    };
    timer = window.setTimeout(() => void poll(), delay);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [google, device, busy, provider.id, credentialId, refresh, callbackForm]);

  async function start(credential: ProviderCredential) {
    setLoginCredential(credential);
    setBusy(true);
    setError("");
    setNotice("");
    setGoogle(null);
    setDevice(null);
    callbackForm.setFieldValue("callback", "");
    try {
      if (provider.kind === "agy") {
        const result = await startGoogleSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (url.protocol !== "https:" || url.hostname !== "accounts.google.com")
          throw new Error("Invalid Google authorization URL.");
        setGoogle({ ...result, url: url.href });
      } else if (provider.kind === "codex") {
        const result = await startCodexSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (url.protocol !== "https:" || url.hostname !== "auth.openai.com")
          throw new Error("Invalid Codex authorization URL.");
        setDevice({ ...result, url: url.href });
      }
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Card className="mt-6 p-5">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h2 className="break-words text-lg font-semibold">{provider.name}</h2>
          <p className="mt-1 break-all text-sm text-muted-foreground">
            {kind?.label ?? provider.kind} · {provider.id}
          </p>
        </div>
        <div className="flex shrink-0 gap-2">
          {kind &&
            ((kind.api_key && provider.kind !== "zen") ||
              kind.fixed_name === null) && (
              <Button
                variant="outline"
                size="icon"
                aria-label={`Edit ${provider.name}`}
                disabled={busy || deleting}
                onClick={onEdit}
              >
                <Pencil />
              </Button>
            )}
          <Button
            variant="destructive"
            size="icon"
            aria-label={
              deleting ? `Deleting ${provider.name}` : `Delete ${provider.name}`
            }
            disabled={busy || deleting}
            onClick={onDelete}
          >
            <Trash2 />
          </Button>
        </div>
      </div>
      <Credentials
        provider={provider}
        kind={kind}
        onLogin={(credential) => void start(credential)}
        loginBusy={busy || !!google || !!device}
      />
      {kind?.api_key && provider.kind !== "zen" && (
        <p className="mt-2 break-all text-sm text-muted-foreground">
          Upstream: {provider.base_url}
        </p>
      )}
      {provider.kind === "agy" && (
        <p className="mt-3 text-sm text-muted-foreground">
          Configure your Google OAuth client in Settings before signing in.
        </p>
      )}
      {error && (
        <Alert variant="destructive" className="mt-4">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      {notice && (
        <p role="status" className="mt-4 text-sm">
          {notice}
        </p>
      )}
      {(google || device) && (
        <p className="mt-4 text-sm">Signing in: {loginCredential?.name}</p>
      )}
      {google && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void callbackForm.handleSubmit();
          }}
          className="mt-4 grid gap-4"
        >
          <p className="text-sm">
            Open{" "}
            <a
              className="underline"
              href={google.url}
              target="_blank"
              rel="noopener noreferrer"
            >
              Google authorization
            </a>{" "}
            and sign in.{" "}
            {google.local_callback
              ? "Waiting for the local callback. If the callback is on another machine, copy its URL below instead."
              : "Copy the entire redirected URL from the browser address bar; the loopback page may not load."}
          </p>
          <callbackForm.Field name="callback">
            {(field) => (
              <Label className="block text-sm font-medium">
                Redirected callback URL
                <Input
                  className={inputClass}
                  required
                  type="url"
                  autoComplete="off"
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => field.handleChange(event.target.value)}
                />
              </Label>
            )}
          </callbackForm.Field>
          <p className="text-xs text-muted-foreground">
            The callback URL contains a temporary authorization code. Do not
            share it.
          </p>
          <div className="flex gap-2">
            <Button type="submit" disabled={busy}>
              Finish Google sign-in
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => {
                setGoogle(null);
                callbackForm.setFieldValue("callback", "");
              }}
            >
              Cancel
            </Button>
          </div>
        </form>
      )}
      {device && (
        <div className="mt-4 grid gap-3 text-sm">
          <p>
            Open{" "}
            <a
              className="underline"
              href={device.url}
              target="_blank"
              rel="noopener noreferrer"
            >
              OpenAI device authorization
            </a>{" "}
            and enter this code:
          </p>
          <code className="select-all text-lg font-semibold">
            {device.user_code}
          </code>
          <p role="status" className="text-muted-foreground">
            Waiting for authorization…
          </p>
          <div>
            <Button variant="outline" onClick={() => setDevice(null)}>
              Cancel
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}
