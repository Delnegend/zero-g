import { useForm } from "@tanstack/react-form";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { message } from "@/server/api/client";
import { finishClaudeSignIn } from "@/server/api/providers";
import type { CallbackFlow } from "./signInFlow";

export function CallbackSignIn({
  providerId,
  flow,
  busy,
  setBusy,
  onError,
  onComplete,
  onCancel,
}: {
  providerId: string;
  flow: CallbackFlow;
  busy: boolean;
  setBusy: (busy: boolean) => void;
  onError: (error: string) => void;
  onComplete: () => void;
  onCancel: () => void;
}) {
  const form = useForm({
    defaultValues: { callback: "" },
    onSubmit: async ({ value }) => {
      setBusy(true);
      onError("");
      try {
        await finishClaudeSignIn(
          providerId,
          flow.credential.id,
          flow.data.id,
          value.callback,
        );
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
          Claude authorization
        </a>{" "}
        and sign in. Paste the authorization code or redirected callback URL below.
      </p>
      <form.Field name="callback">
        {(field) => (
          <Label className="block text-sm font-medium">
            Authorization code or callback URL
            <Input
              className="mt-1"
              required
              type="text"
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
          Finish Claude sign-in
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
