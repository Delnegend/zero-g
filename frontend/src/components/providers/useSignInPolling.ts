import { useEffect } from "react";
import { message } from "@/server/api/client";
import { pollSignIn } from "@/server/api/providers";
import type { PollingFlow } from "./signInFlow";

export function useSignInPolling({
  providerId,
  providerKind,
  flow,
  onComplete,
  onFailure,
}: {
  providerId: string;
  providerKind: string;
  flow: PollingFlow | null;
  onComplete: () => void;
  onFailure: (error: string) => void;
}) {
  useEffect(() => {
    if (!flow) return;
    const controller = new AbortController();
    const kind =
      flow.kind === "google"
        ? "agy"
        : flow.kind === "cursor"
          ? "cursor"
          : providerKind === "codex"
            ? "codex"
            : providerKind === "grok-build"
              ? "grok-build"
              : "grok";
    const delay =
      flow.kind === "google" ? 2000 : Math.max(1, flow.data.interval) * 1000;
    let timer: number;
    const poll = async () => {
      try {
        const result = await pollSignIn(
          providerId,
          flow.credential.id,
          kind,
          flow.data.id,
          controller.signal,
        );
        if (controller.signal.aborted) return;
        if (result.status === "complete") {
          onComplete();
        } else if (result.status === "pending") {
          timer = window.setTimeout(() => void poll(), delay);
        } else {
          onFailure(
            ("error" in result &&
              typeof result.error === "string" &&
              result.error) ||
              "Sign-in failed.",
          );
        }
      } catch (err) {
        if (!controller.signal.aborted) onFailure(message(err));
      }
    };
    timer = window.setTimeout(() => void poll(), delay);
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [flow, providerId, providerKind, onComplete, onFailure]);
}
