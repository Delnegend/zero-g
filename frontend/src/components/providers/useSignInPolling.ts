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
    const kind = flow.kind === "cursor" ? "cursor" : "codex";
    const delay = Math.max(1, flow.data.interval) * 1000;
    const timer = { id: 0 };
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
          timer.id = window.setTimeout(() => void poll(), delay);
        }
      } catch (err) {
        if (!controller.signal.aborted) onFailure(message(err));
      }
    };
    timer.id = window.setTimeout(() => void poll(), delay);
    return () => {
      controller.abort();
      window.clearTimeout(timer.id);
    };
  }, [flow, providerId, providerKind, onComplete, onFailure]);
}
