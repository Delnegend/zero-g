import { useState } from "react";
import { useForm } from "@tanstack/react-form";
import { useQueryClient } from "@tanstack/react-query";
import {
  deleteConsoleSession,
  deleteOpenrouterManagementKey,
  credentialUsageOptions,
  saveConsoleSession,
  saveOpenrouterManagementKey,
} from "@/server/api/providers";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";

export function CredentialConfiguration({
  providerId,
  credentialId,
  credentialName,
  kind,
}: {
  providerId: string;
  credentialId: string;
  credentialName: string;
  kind: "zen" | "openrouter";
}) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <Button variant="outline" onClick={() => setOpen(true)}>
        {kind === "zen" ? "Configure console" : "Configure management key"}
      </Button>
      {open && (
        <ConsoleDialog
          providerId={providerId}
          credentialId={credentialId}
          credentialName={credentialName}
          kind={kind}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}
type DialogState =
  | { status: "idle" }
  | { status: "working" }
  | { status: "error"; message: string };

function ConsoleDialog({
  providerId,
  credentialId,
  credentialName,
  kind,
  onClose,
}: {
  providerId: string;
  credentialId: string;
  credentialName: string;
  kind: "zen" | "openrouter";
  onClose: () => void;
}) {
  const queryClient = useQueryClient();
  const [state, setState] = useState<DialogState>({ status: "idle" });
  const busy = state.status === "working";
  const error = state.status === "error" ? state.message : null;
  const isZen = kind === "zen";
  const refresh = () =>
    queryClient.invalidateQueries({
      queryKey: credentialUsageOptions(providerId, credentialId).queryKey,
    });
  const form = useForm({
    defaultValues: { workspace_id: "", session: "", management_key: "" },
    onSubmit: async ({ value }) => {
      setState({ status: "working" });
      try {
        if (isZen) {
          await saveConsoleSession(providerId, credentialId, {
            workspace_id: value.workspace_id,
            session: value.session,
          });
        } else {
          await saveOpenrouterManagementKey(providerId, credentialId, {
            management_key: value.management_key,
          });
        }
        await refresh();
        onClose();
      } catch {
        setState({
          status: "error",
          message: isZen
            ? "Could not save the console session. Check the workspace ID and try again."
            : "Could not save the management key. Try again.",
        });
      }
    },
  });
  async function remove() {
    setState({ status: "working" });
    try {
      if (isZen) {
        await deleteConsoleSession(providerId, credentialId);
      } else {
        await deleteOpenrouterManagementKey(providerId, credentialId);
      }
      await refresh();
      onClose();
    } catch {
      setState({
        status: "error",
        message: isZen
          ? "Could not remove the console session. Try again."
          : "Could not remove the management key. Try again.",
      });
    }
  }

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
    >
      <DialogContent
        showCloseButton={!busy}
        className="max-h-[calc(100dvh-2rem)] overflow-y-auto"
      >
        <form
          className="space-y-4"
          onSubmit={(event) => {
            event.preventDefault();
            event.stopPropagation();
            void form.handleSubmit();
          }}
        >
          <DialogHeader>
            <DialogTitle>
              {isZen ? "Zen console" : "OpenRouter management key"} for{" "}
              {credentialName}
            </DialogTitle>
            <DialogDescription>
              {isZen
                ? "Associate this credential with your Zen Console workspace to view its balance. Enter the workspace ID and the value of the __Host-console_session cookie, not an API key. The session is stored encrypted, expires, and may need to be replaced."
                : "Enter a separate OpenRouter management key to view the account's remaining credits. An inference API key cannot access account credits. The management key is stored encrypted."}
            </DialogDescription>
          </DialogHeader>
          {error && (
            <Alert variant="destructive">
              <AlertDescription>{error}</AlertDescription>
            </Alert>
          )}
          {isZen ? (
            <>
              <form.Field name="workspace_id">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Workspace ID
                    <Input
                      className="mt-1"
                      required
                      disabled={busy}
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) =>
                        field.handleChange(event.target.value)
                      }
                    />
                  </Label>
                )}
              </form.Field>
              <form.Field name="session">
                {(field) => (
                  <Label className="block text-sm font-medium">
                    Console session
                    <Input
                      className="mt-1"
                      type="password"
                      autoComplete="new-password"
                      required
                      disabled={busy}
                      value={field.state.value}
                      onBlur={field.handleBlur}
                      onChange={(event) =>
                        field.handleChange(event.target.value)
                      }
                    />
                  </Label>
                )}
              </form.Field>
            </>
          ) : (
            <form.Field name="management_key">
              {(field) => (
                <Label className="block text-sm font-medium">
                  Management key
                  <Input
                    className="mt-1"
                    type="password"
                    autoComplete="new-password"
                    required
                    disabled={busy}
                    value={field.state.value}
                    onBlur={field.handleBlur}
                    onChange={(event) => field.handleChange(event.target.value)}
                  />
                </Label>
              )}
            </form.Field>
          )}
          <div className="flex flex-wrap gap-2">
            <Button type="submit" disabled={busy}>
              {busy
                ? "Working…"
                : isZen
                  ? "Save session"
                  : "Save management key"}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={() => void remove()}
            >
              {isZen ? "Remove session" : "Remove management key"}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={busy}
              onClick={onClose}
            >
              Cancel
            </Button>
          </div>
        </form>
      </DialogContent>
    </Dialog>
  );
}
