import { useForm } from "@tanstack/react-form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { message } from "@/server/api/client";
import { finishClaudeSignIn, finishGoogleSignIn } from "@/server/api/providers";
import { inputClass } from "./inputClass";
import type { CallbackFlow } from "./signInFlow";
import { useSignInPolling } from "./useSignInPolling";

export function CallbackSignIn({
  providerId,
  flow,
  busy,
  setBusy,
  onError,
  onFailure,
  onComplete,
  onCancel,
}: {
  providerId: string;
  flow: CallbackFlow;
  busy: boolean;
  setBusy: (busy: boolean) => void;
  onError: (error: string) => void;
  onFailure: (error: string) => void;
  onComplete: () => void;
  onCancel: () => void;
}) {
  useSignInPolling({
    providerId,
    providerKind: "agy",
    flow: flow.kind === "google" && !busy ? flow : null,
    onComplete,
    onFailure,
  });
  const form = useForm({
    defaultValues: { callback: "" },
    onSubmit: async ({ value }) => {
      setBusy(true);
      onError("");
      try {
        if (flow.kind === "claude") {
          await finishClaudeSignIn(
            providerId,
            flow.credential.id,
            flow.data.id,
            value.callback,
          );
        } else {
          await finishGoogleSignIn(
            providerId,
            flow.credential.id,
            value.callback,
          );
        }
        onComplete();
      } catch (err) {
        onError(message(err));
      } finally {
        setBusy(false);
      }
    },
  });
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void form.handleSubmit();
      }}
      className="mt-4 grid gap-4"
    >
      <p className="text-sm">
        Open{" "}
        <a
          className="underline"
          href={flow.data.url}
          target="_blank"
          rel="noopener noreferrer"
        >
          {flow.kind === "google" ? "Google" : "Claude"} authorization
        </a>{" "}
        and sign in.{" "}
        {flow.kind === "google"
          ? flow.data.local_callback
            ? "Waiting for the local callback. If the callback is on another machine, copy its URL below instead."
            : "Copy the entire redirected URL from the browser address bar; the loopback page may not load."
          : "Paste the authorization code or redirected callback URL below."}
      </p>
      <form.Field name="callback">
        {(field) => (
          <Label className="block text-sm font-medium">
            {flow.kind === "google"
              ? "Redirected callback URL"
              : "Authorization code or callback URL"}
            <Input
              className={inputClass}
              required
              type={flow.kind === "google" ? "url" : "text"}
              autoComplete="off"
              value={field.state.value}
              onBlur={field.handleBlur}
              onChange={(event) => field.handleChange(event.target.value)}
            />
          </Label>
        )}
      </form.Field>
      <p className="text-xs text-muted-foreground">
        The authorization code or callback URL is temporary and sensitive. Do
        not share it.
      </p>
      <div className="flex gap-2">
        <Button type="submit" disabled={busy}>
          Finish {flow.kind === "google" ? "Google" : "Claude"} sign-in
        </Button>
        <Button
          type="button"
          variant="outline"
          disabled={busy}
          onClick={onCancel}
        >
          Cancel
        </Button>
      </div>
    </form>
  );
}
