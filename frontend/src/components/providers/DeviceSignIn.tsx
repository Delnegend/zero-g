import { Button } from "@/components/ui/button";
import { CopyDeviceCode } from "./CopyDeviceCode";
import type { SignInFlow } from "./signInFlow";
import { useSignInPolling } from "./useSignInPolling";

export function DeviceSignIn({
  providerId,
  providerKind,
  flow,
  onError,
  onFailure,
  onComplete,
  onCancel,
}: {
  providerId: string;
  providerKind: string;
  flow: Extract<SignInFlow, { kind: "device" }>;
  onError: (error: string) => void;
  onFailure: (error: string) => void;
  onComplete: () => void;
  onCancel: () => void;
}) {
  useSignInPolling({ providerId, providerKind, flow, onComplete, onFailure });
  return (
    <div className="mt-4 grid gap-3 text-sm">
      <p>
        Open{" "}
        <a
          className="underline"
          href={flow.data.url}
          target="_blank"
          rel="noopener noreferrer"
        >
          {providerKind === "codex" ? "OpenAI" : "Grok"} device authorization
        </a>{" "}
        and enter this code:
      </p>
      <CopyDeviceCode
        code={flow.data.user_code}
        onError={() => onError("Could not copy the device code.")}
      />
      <p role="status" className="text-muted-foreground">
        Waiting for authorization…
      </p>
      <div>
        <Button variant="outline" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}
