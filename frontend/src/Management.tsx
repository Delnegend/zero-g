import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type FormEvent,
} from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "./components/ui/button";
import { Input } from "./components/ui/input";
import { Textarea } from "./components/ui/textarea";
import { Checkbox } from "./components/ui/checkbox";
import { Alert, AlertDescription } from "./components/ui/alert";
import { Card } from "./components/ui/card";
import { Label } from "./components/ui/label";
import {
  getSettings,
  updateSettings,
  type Settings,
  type SettingsUpdate,
} from "./api/settings";

type VirtualKey = {
  id: string;
  name: string;
  providers: string[];
  models: string[];
  created_at: number;
};

const inputClass =
  "mt-1 h-9 w-full rounded-lg border border-input bg-background px-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";
const panelClass = "p-5";

async function request<T>(url: string, options?: RequestInit): Promise<T> {
  const response = await fetch(url, options);
  if (!response.ok) {
    const message = await response.text();
    throw new Error(message || `Request failed (${response.status})`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : "Request failed";
}

function SettingsPage() {
  const [draft, setSettings] = useState<Settings | null>(null);
  const [zenKey, setZenKey] = useState("");
  const [openaiKey, setOpenaiKey] = useState("");
  const [oauthSecret, setOauthSecret] = useState("");
  const [clearZen, setClearZen] = useState(false);
  const [clearOpenai, setClearOpenai] = useState(false);
  const [clearOauth, setClearOauth] = useState(false);
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const saveController = useRef<AbortController | null>(null);

  const settingsQuery = useQuery({
    queryKey: ["settings"],
    retry: false,
    queryFn: ({ signal }) => getSettings(signal),
  });
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

  const settings = draft ?? settingsQuery.data ?? null;
  const displayError =
    error || (settingsQuery.error ? message(settingsQuery.error) : "");

  useEffect(() => () => saveController.current?.abort(), []);

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!settings) return;
    saveController.current?.abort();
    const controller = new AbortController();
    saveController.current = controller;
    setBusy(true);
    setError("");
    setSaved(false);
    try {
      const body: SettingsUpdate = {
        log_retention_days: settings.log_retention_days,
        zen_url: settings.zen_url,
        openai_base_url: settings.openai_base_url,
        oauth_client_id: settings.oauth_client_id,
      };
      if (clearZen || zenKey) body.zen_api_key = clearZen ? "" : zenKey;
      if (clearOpenai || openaiKey)
        body.openai_api_key = clearOpenai ? "" : openaiKey;
      if (clearOauth || oauthSecret)
        body.oauth_client_secret = clearOauth ? "" : oauthSecret;
      const updated = await saveMutation.mutateAsync({
        body,
        signal: controller.signal,
      });
      if (controller.signal.aborted) return;
      await queryClient.cancelQueries({ queryKey: ["settings"] });
      if (controller.signal.aborted) return;
      setZenKey((current) => (current === zenKey ? "" : current));
      setOpenaiKey((current) => (current === openaiKey ? "" : current));
      setOauthSecret((current) => (current === oauthSecret ? "" : current));
      setClearZen((current) => (current === clearZen ? false : current));
      setClearOpenai((current) => (current === clearOpenai ? false : current));
      setClearOauth((current) => (current === clearOauth ? false : current));
      setSettings((current) => {
        if (!current) return null;
        const logRetention =
          current.log_retention_days === settings.log_retention_days
            ? updated.log_retention_days
            : current.log_retention_days;
        const zenUrl =
          current.zen_url === settings.zen_url
            ? updated.zen_url
            : current.zen_url;
        const openaiBaseUrl =
          current.openai_base_url === settings.openai_base_url
            ? updated.openai_base_url
            : current.openai_base_url;
        const oauthClientId =
          current.oauth_client_id === settings.oauth_client_id
            ? updated.oauth_client_id
            : current.oauth_client_id;
        if (
          logRetention === updated.log_retention_days &&
          zenUrl === updated.zen_url &&
          openaiBaseUrl === updated.openai_base_url &&
          oauthClientId === updated.oauth_client_id
        )
          return null;
        return {
          ...updated,
          log_retention_days: logRetention,
          zen_url: zenUrl,
          openai_base_url: openaiBaseUrl,
          oauth_client_id: oauthClientId,
        };
      });
      queryClient.setQueryData(["settings"], updated);
      setSaved(true);
    } catch (err) {
      if (!controller.signal.aborted) setError(message(err));
    } finally {
      saveMutation.reset();
      if (!controller.signal.aborted) setBusy(false);
    }
  }

  return (
    <main className="mx-auto max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Settings</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Configure the gateway and provider credentials. Some changes may require
        a restart.
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
      {settings && (
        <Card className="mt-6 max-w-2xl p-5">
          <form onSubmit={(event) => void save(event)} className="grid gap-5">
            <Label className="block text-sm font-medium">
              Log retention (days)
              <Input
                className={inputClass}
                type="number"
                min={1}
                max={36500}
                required
                value={settings.log_retention_days}
                onChange={(e) =>
                  setSettings({
                    ...settings,
                    log_retention_days: Number(e.target.value),
                  })
                }
              />
            </Label>
            <Label className="block text-sm font-medium">
              Zen URL
              <Input
                className={inputClass}
                type="url"
                value={settings.zen_url}
                onChange={(e) =>
                  setSettings({ ...settings, zen_url: e.target.value })
                }
              />
            </Label>
            <Label className="block text-sm font-medium">
              Zen API key{" "}
              <span className="font-normal text-muted-foreground">
                (
                {settings.zen_api_key_set
                  ? "configured; leave blank to retain"
                  : "not configured"}
                )
              </span>
              <Input
                className={inputClass}
                type="password"
                autoComplete="new-password"
                disabled={clearZen}
                value={zenKey}
                onChange={(e) => setZenKey(e.target.value)}
              />
            </Label>
            {settings.zen_api_key_set && (
              <Label htmlFor="clear-zen" className="text-sm">
                <Checkbox
                  id="clear-zen"
                  checked={clearZen}
                  onCheckedChange={setClearZen}
                />
                Clear Zen API key
              </Label>
            )}
            <Label className="block text-sm font-medium">
              OpenAI-compatible base URL
              <Input
                className={inputClass}
                type="url"
                placeholder="https://api.openai.com/v1"
                value={settings.openai_base_url}
                onChange={(e) =>
                  setSettings({ ...settings, openai_base_url: e.target.value })
                }
              />
            </Label>
            <Label className="block text-sm font-medium">
              OpenAI-compatible API key{" "}
              <span className="font-normal text-muted-foreground">
                (
                {settings.openai_api_key_set
                  ? "configured; leave blank to retain"
                  : "not configured; upstream requests will have no bearer token"}
                )
              </span>
              <Input
                className={inputClass}
                type="password"
                autoComplete="new-password"
                disabled={clearOpenai}
                value={openaiKey}
                onChange={(e) => setOpenaiKey(e.target.value)}
              />
            </Label>
            {settings.openai_api_key_set && (
              <Label htmlFor="clear-openai" className="text-sm">
                <Checkbox
                  id="clear-openai"
                  checked={clearOpenai}
                  onCheckedChange={setClearOpenai}
                />
                Clear OpenAI-compatible API key
              </Label>
            )}
            <Label className="block text-sm font-medium">
              Google OAuth client ID
              <Input
                className={inputClass}
                value={settings.oauth_client_id}
                onChange={(e) =>
                  setSettings({ ...settings, oauth_client_id: e.target.value })
                }
              />
            </Label>
            <Label className="block text-sm font-medium">
              Google OAuth client secret{" "}
              <span className="font-normal text-muted-foreground">
                (
                {settings.oauth_client_secret_set
                  ? "configured; leave blank to retain"
                  : "not configured"}
                )
              </span>
              <Input
                className={inputClass}
                type="password"
                autoComplete="new-password"
                disabled={clearOauth}
                value={oauthSecret}
                onChange={(e) => setOauthSecret(e.target.value)}
              />
            </Label>
            {settings.oauth_client_secret_set && (
              <Label htmlFor="clear-oauth" className="text-sm">
                <Checkbox
                  id="clear-oauth"
                  checked={clearOauth}
                  onCheckedChange={setClearOauth}
                />
                Clear Google OAuth client secret
              </Label>
            )}
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

function KeysPage() {
  const queryClient = useQueryClient();
  const keysQuery = useQuery({
    queryKey: ["virtual-keys"],
    retry: false,
    queryFn: ({ signal }) =>
      request<{ keys: VirtualKey[] }>("/api/keys", { signal }),
  });
  const keys = keysQuery.data?.keys ?? [];
  const createMutation = useMutation({
    gcTime: 0,
    mutationFn: (body: {
      name: string;
      providers: string[];
      models: string[];
    }) =>
      request<VirtualKey & { key: string }>("/api/keys", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      }),
  });
  const revokeMutation = useMutation({
    mutationFn: (id: string) =>
      request(`/api/keys/${encodeURIComponent(id)}`, { method: "DELETE" }),
  });
  const [name, setName] = useState("");
  const [providers, setProviders] = useState<string[]>([]);
  const [models, setModels] = useState("");
  const [newKey, setNewKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const loading = keysQuery.isPending;
  const displayError =
    error || (keysQuery.error ? message(keysQuery.error) : "");

  async function create(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    setNewKey("");
    try {
      const result = await createMutation.mutateAsync({
        name,
        providers,
        models: models
          .split(/[\n,]/)
          .map((model) => model.trim())
          .filter(Boolean),
      });
      setNewKey(result.key);
      setName("");
      setProviders([]);
      setModels("");
      await queryClient.invalidateQueries(
        { queryKey: ["virtual-keys"] },
        { throwOnError: true },
      );
    } catch (err) {
      setError(message(err));
    } finally {
      createMutation.reset();
      setBusy(false);
    }
  }
  async function revoke(key: VirtualKey) {
    if (
      !window.confirm(
        `Revoke and delete key “${key.name}”? This cannot be undone.`,
      )
    )
      return;
    setBusy(true);
    setError("");
    try {
      await revokeMutation.mutateAsync(key.id);
      await queryClient.cancelQueries({ queryKey: ["virtual-keys"] });
      queryClient.setQueryData<{ keys: VirtualKey[] }>(
        ["virtual-keys"],
        (current) =>
          current && {
            ...current,
            keys: current.keys.filter((item) => item.id !== key.id),
          },
      );
      await queryClient.invalidateQueries(
        { queryKey: ["virtual-keys"] },
        { throwOnError: true },
      );
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }
  return (
    <main className="mx-auto max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Virtual keys</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Create keys for inference access and restrict their providers and
        models.
      </p>
      {displayError && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{displayError}</AlertDescription>
        </Alert>
      )}
      {newKey && (
        <Card role="status" className="mt-6 border border-primary p-5">
          <h2 className="font-semibold">Copy your new key now</h2>
          <p className="mt-1 text-sm text-muted-foreground">
            It will not be shown again.
          </p>
          <code className="mt-3 block break-all select-all">{newKey}</code>
          <Button
            className="mt-3"
            variant="outline"
            onClick={() => setNewKey("")}
          >
            Dismiss
          </Button>
        </Card>
      )}
      <Card className="mt-6 max-w-2xl p-5">
        <form onSubmit={(event) => void create(event)} className="grid gap-5">
          <h2 className="text-lg font-semibold">Create key</h2>
          <Label className="block text-sm font-medium">
            Name
            <Input
              className={inputClass}
              required
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </Label>
          <fieldset>
            <legend className="text-sm font-medium">Providers</legend>
            <p className="mt-1 text-xs text-muted-foreground">
              Select none to allow all providers.
            </p>
            <div className="mt-2 flex flex-wrap gap-4">
              {["agy", "codex", "zen", "openai"].map((provider) => (
                <Label
                  key={provider}
                  htmlFor={`provider-${provider}`}
                  className="text-sm"
                >
                  <Checkbox
                    id={`provider-${provider}`}
                    checked={providers.includes(provider)}
                    onCheckedChange={(checked) =>
                      setProviders(
                        checked
                          ? [...providers, provider]
                          : providers.filter((item) => item !== provider),
                      )
                    }
                  />
                  {provider}
                </Label>
              ))}
            </div>
          </fieldset>
          <Label className="block text-sm font-medium">
            Models{" "}
            <span className="font-normal text-muted-foreground">
              (comma or newline separated; blank allows all)
            </span>
            <Textarea
              className={`${inputClass} h-24 py-2`}
              value={models}
              onChange={(e) => setModels(e.target.value)}
            />
          </Label>
          <div>
            <Button type="submit" disabled={busy}>
              {busy ? "Working…" : "Create key"}
            </Button>
          </div>
        </form>
      </Card>
      <Card className={`${panelClass} mt-6`}>
        <h2 className="text-lg font-semibold">Existing keys</h2>
        {loading ? (
          <p className="mt-4 text-sm text-muted-foreground">Loading keys…</p>
        ) : keys.length === 0 ? (
          <p className="mt-4 text-sm text-muted-foreground">
            No keys created yet.
          </p>
        ) : (
          <ul className="mt-4 divide-y">
            {keys.map((key) => (
              <li
                key={key.id}
                className="flex flex-wrap items-center justify-between gap-4 py-4"
              >
                <div>
                  <h3 className="font-medium">{key.name}</h3>
                  <p className="mt-1 break-all text-sm text-muted-foreground">
                    Providers:{" "}
                    {key.providers.length ? key.providers.join(", ") : "All"} ·
                    Models: {key.models.length ? key.models.join(", ") : "All"}
                  </p>
                </div>
                <Button
                  variant="outline"
                  disabled={busy}
                  onClick={() => void revoke(key)}
                >
                  Revoke / delete
                </Button>
              </li>
            ))}
          </ul>
        )}
      </Card>
    </main>
  );
}

function ProvidersPage() {
  const [googleUrl, setGoogleUrl] = useState("");
  const [googleFlow, setGoogleFlow] = useState<{
    id: string;
    local_callback: boolean;
  } | null>(null);
  const [callback, setCallback] = useState("");
  const [device, setDevice] = useState<{
    id: string;
    url: string;
    user_code: string;
    interval: number;
  } | null>(null);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const statusQuery = useQuery({
    queryKey: ["provider-status"],
    retry: false,
    queryFn: ({ signal }) =>
      request<{ agy: boolean; codex: boolean }>("/api/providers/status", {
        signal,
      }),
  });
  const status = statusQuery.data ?? null;
  const queryClient = useQueryClient();
  const refreshStatus = useCallback(async () => {
    try {
      await queryClient.fetchQuery({
        queryKey: ["provider-status"],
        queryFn: ({ signal }) =>
          request<{ agy: boolean; codex: boolean }>("/api/providers/status", {
            signal,
          }),
        staleTime: 0,
      });
    } catch (err) {
      setError(message(err));
    }
  }, [queryClient]);

  const displayError =
    error || (statusQuery.error ? message(statusQuery.error) : "");

  useEffect(() => {
    if (!googleFlow || busy) return;
    const controller = new AbortController();
    let timer: number;
    const poll = async () => {
      try {
        const result = await request<{ status: string; error?: string }>(
          "/api/providers/agy/poll",
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ id: googleFlow.id }),
            signal: controller.signal,
          },
        );
        if (controller.signal.aborted) return;
        if (result.status === "complete") {
          setGoogleFlow(null);
          setGoogleUrl("");
          setCallback("");
          void refreshStatus();
          setNotice("Antigravity sign-in complete.");
        } else if (result.status === "pending")
          timer = window.setTimeout(() => void poll(), 2000);
        else {
          setGoogleFlow(null);
          setError(result.error || "Google sign-in failed.");
        }
      } catch (err) {
        if (!controller.signal.aborted) {
          setGoogleFlow(null);
          setError(message(err));
        }
      }
    };
    timer = window.setTimeout(() => void poll(), 2000);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [googleFlow, busy, refreshStatus]);

  useEffect(() => {
    if (!device) return;
    const controller = new AbortController();
    let timer: number;
    const poll = async () => {
      try {
        const result = await request<{ status: string }>(
          "/api/providers/codex/poll",
          {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ id: device.id }),
            signal: controller.signal,
          },
        );
        if (controller.signal.aborted) return;
        if (result.status === "complete") {
          setDevice(null);
          void refreshStatus();
          setNotice("Codex sign-in complete.");
        } else if (result.status === "pending")
          timer = window.setTimeout(
            () => void poll(),
            Math.max(1, device.interval) * 1000,
          );
        else {
          setDevice(null);
          setError("Unexpected device sign-in status.");
        }
      } catch (err) {
        if (!controller.signal.aborted) {
          setDevice(null);
          setError(message(err));
        }
      }
    };
    timer = window.setTimeout(
      () => void poll(),
      Math.max(1, device.interval) * 1000,
    );
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [device, refreshStatus]);

  async function startGoogle() {
    setBusy(true);
    setError("");
    setNotice("");
    setGoogleUrl("");
    setGoogleFlow(null);
    setCallback("");
    try {
      const result = await request<{
        id: string;
        url: string;
        local_callback: boolean;
      }>("/api/providers/agy/start", { method: "POST" });
      const url = new URL(result.url);
      if (url.protocol !== "https:" || url.hostname !== "accounts.google.com")
        throw new Error("Invalid Google authorization URL.");
      setGoogleUrl(url.href);
      setGoogleFlow({ id: result.id, local_callback: result.local_callback });
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }
  async function finishGoogle(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError("");
    setNotice("");
    try {
      await request("/api/providers/agy/finish", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ callback_url: callback }),
      });
      setGoogleFlow(null);
      setGoogleUrl("");
      setCallback("");
      void refreshStatus();
      setNotice("Antigravity sign-in complete.");
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }
  async function startCodex() {
    setBusy(true);
    setError("");
    setNotice("");
    setDevice(null);
    try {
      const result = await request<{
        id: string;
        url: string;
        user_code: string;
        interval: number;
      }>("/api/providers/codex/start", { method: "POST" });
      const url = new URL(result.url);
      if (url.protocol !== "https:" || url.hostname !== "auth.openai.com")
        throw new Error("Invalid Codex authorization URL.");
      setDevice({ ...result, url: url.href });
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="mx-auto max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <h1 className="text-3xl font-semibold tracking-tight">Providers</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Sign in to connect provider accounts to the gateway.
      </p>
      {displayError && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{displayError}</AlertDescription>
        </Alert>
      )}
      {notice && (
        <p role="status" className="mt-5 text-sm">
          {notice}
        </p>
      )}
      <Card className={`${panelClass} mt-6 max-w-2xl`}>
        <h2 className="text-lg font-semibold">Antigravity (Google)</h2>
        <p className="mt-2 text-sm">
          {status
            ? status.agy
              ? "Credentials configured"
              : "Not signed in"
            : "Credential status unavailable"}
        </p>
        <p className="mt-2 text-sm text-muted-foreground">
          Configure your Google OAuth client in Settings before signing in.
        </p>
        <Button
          className="mt-4"
          disabled={busy}
          onClick={() => void startGoogle()}
        >
          Start Google sign-in
        </Button>
        {googleUrl && (
          <form
            onSubmit={(event) => void finishGoogle(event)}
            className="mt-4 grid gap-4"
          >
            <p className="text-sm">
              Open{" "}
              <a
                className="underline"
                href={googleUrl}
                target="_blank"
                rel="noopener noreferrer"
              >
                Google authorization
              </a>{" "}
              and sign in.{" "}
              {googleFlow?.local_callback
                ? "Waiting for the local callback. If the callback is on another machine, copy its URL below instead."
                : "Copy the entire redirected URL from the browser address bar; the loopback page may not load."}
            </p>
            <Label className="block text-sm font-medium">
              Redirected callback URL
              <Input
                className={inputClass}
                required
                type="url"
                autoComplete="off"
                value={callback}
                onChange={(e) => setCallback(e.target.value)}
              />
            </Label>
            <p className="text-xs text-muted-foreground">
              The callback URL contains a temporary authorization code. Do not
              share it.
            </p>
            <div>
              <Button type="submit" disabled={busy}>
                Finish Google sign-in
              </Button>
            </div>
          </form>
        )}
      </Card>
      <Card className={`${panelClass} mt-6 max-w-2xl`}>
        <h2 className="text-lg font-semibold">Codex (OpenAI)</h2>
        <p className="mt-2 text-sm">
          {status
            ? status.codex
              ? "Credentials configured"
              : "Not signed in"
            : "Credential status unavailable"}
        </p>
        <Button
          className="mt-4"
          disabled={busy || !!device}
          onClick={() => void startCodex()}
        >
          Start Codex sign-in
        </Button>
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
    </main>
  );
}

export function Management({
  page,
}: {
  page: "settings" | "keys" | "providers";
}) {
  return page === "settings" ? (
    <SettingsPage />
  ) : page === "keys" ? (
    <KeysPage />
  ) : (
    <ProvidersPage />
  );
}
