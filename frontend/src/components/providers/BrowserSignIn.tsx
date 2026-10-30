import { Button } from "@/components/ui/button";
import type { SignInFlow } from "./signInFlow";
import { useSignInPolling } from "./useSignInPolling";

export function BrowserSignIn({
  providerId,
  flow,
  onFailure,
  onComplete,
  onCancel,
}: {
  providerId: string;
  flow: Extract<SignInFlow, { kind: "cursor" }>;
  onFailure: (error: string) => void;
  onComplete: () => void;
  onCancel: () => void;
}) {
  useSignInPolling({
    providerId,
    providerKind: "cursor",
    flow,
    onComplete,
    onFailure,
  });
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
          Cursor authorization
        </a>{" "}
        and sign in.
      </p>
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
