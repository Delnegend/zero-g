import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useState, type ReactNode } from "react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { message } from "@/server/api/client";
import {
  credentialsOptions,
  credentialUsageKey,
  startCodexSignIn,
  startGoogleSignIn,
  startClaudeSignIn,
  startCursorSignIn,
  startGrokSignIn,
  type Provider,
  type ProviderCredential,
} from "@/server/api/providers";
import { BrowserSignIn } from "./BrowserSignIn";
import { CallbackSignIn } from "./CallbackSignIn";
import { DeviceSignIn } from "./DeviceSignIn";
import type { SignInFlow } from "./signInFlow";

export function ProviderSignIn({
  provider,
  children,
}: {
  provider: Provider;
  children: (actions: {
    busy: boolean;
    loginBusy: boolean;
    start: (credential: ProviderCredential) => void;
  }) => ReactNode;
}) {
  const queryClient = useQueryClient();
  const [flow, setFlow] = useState<SignInFlow | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const refresh = useCallback(() => {
    void queryClient
      .invalidateQueries({ queryKey: credentialsOptions(provider.id).queryKey })
      .then(() =>
        queryClient.invalidateQueries({
          queryKey: credentialUsageKey(provider.id),
        }),
      );
  }, [queryClient, provider.id]);
  const onComplete = useCallback(() => {
    setFlow(null);
    refresh();
    setNotice("Sign-in complete.");
  }, [refresh]);
  const onFailure = useCallback((error: string) => {
    setFlow(null);
    setError(error);
  }, []);

  async function start(credential: ProviderCredential) {
    setBusy(true);
    setError("");
    setNotice("");
    setFlow(null);
    try {
      if (provider.kind === "agy") {
        const result = await startGoogleSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (url.protocol !== "https:" || url.hostname !== "accounts.google.com")
          throw new Error("Invalid Google authorization URL.");
        setFlow({
          kind: "google",
          credential,
          data: { ...result, url: url.href },
        });
      } else if (provider.kind === "codex") {
        const result = await startCodexSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (url.protocol !== "https:" || url.hostname !== "auth.openai.com")
          throw new Error("Invalid Codex authorization URL.");
        setFlow({
          kind: "device",
          credential,
          data: { ...result, url: url.href },
        });
      } else if (provider.kind === "claude") {
        const result = await startClaudeSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (
          url.protocol !== "https:" ||
          url.hostname !== "claude.ai" ||
          url.username ||
          url.password ||
          url.port
        )
          throw new Error("Invalid Claude authorization URL.");
        setFlow({
          kind: "claude",
          credential,
          data: { ...result, url: url.href },
        });
      } else if (provider.kind === "cursor") {
        const result = await startCursorSignIn(provider.id, credential.id);
        const url = new URL(result.url);
        if (
          url.protocol !== "https:" ||
          url.hostname !== "cursor.com" ||
          url.username ||
          url.password ||
          url.port
        )
          throw new Error("Invalid Cursor authorization URL.");
        setFlow({
          kind: "cursor",
          credential,
          data: { ...result, url: url.href },
        });
      } else if (provider.kind === "grok" || provider.kind === "grok-build") {
        const result = await startGrokSignIn(
          provider.id,
          credential.id,
          provider.kind === "grok-build",
        );
        const url = new URL(result.url);
        if (
          url.protocol !== "https:" ||
          !(url.hostname === "x.ai" || url.hostname.endsWith(".x.ai")) ||
          url.username ||
          url.password ||
          url.port
        )
          throw new Error("Invalid Grok authorization URL.");
        setFlow({
          kind: "device",
          credential,
          data: { ...result, url: url.href },
        });
      }
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      {children({
        busy,
        loginBusy: busy || !!flow,
        start: (credential) => void start(credential),
      })}
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
      {flow && (
        <p className="mt-4 text-sm">Signing in: {flow.credential.name}</p>
      )}
      {flow && (flow.kind === "google" || flow.kind === "claude") && (
        <CallbackSignIn
          providerId={provider.id}
          flow={flow}
          busy={busy}
          setBusy={setBusy}
          onError={setError}
          onFailure={onFailure}
          onComplete={onComplete}
          onCancel={() => setFlow(null)}
        />
      )}
      {flow?.kind === "cursor" && (
        <BrowserSignIn
          providerId={provider.id}
          flow={flow}
          onFailure={onFailure}
          onComplete={onComplete}
          onCancel={() => setFlow(null)}
        />
      )}
      {flow?.kind === "device" && (
        <DeviceSignIn
          providerId={provider.id}
          providerKind={provider.kind}
          flow={flow}
          onError={setError}
          onFailure={onFailure}
          onComplete={onComplete}
          onCancel={() => setFlow(null)}
        />
      )}
    </>
  );
}
