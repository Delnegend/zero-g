import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { parseResponseBody } from "@/lib/responseBody";

function format(value: unknown): string {
  if (typeof value === "string") {
    try {
      return JSON.stringify(JSON.parse(value), null, 2);
    } catch {
      return value;
    }
  }
  return JSON.stringify(value, null, 2) ?? "";
}

export function ResponseBody({
  body,
  status,
}: {
  body: string;
  status: number;
}) {
  const [view, setView] = useState<"readable" | "raw">("readable");
  const [copyStatus, setCopyStatus] = useState("");
  const parsed = useMemo(() => parseResponseBody(body), [body]);
  const details = parsed.choices
    .map((choice) =>
      [
        choice.text,
        choice.reasoning && `Reasoning:\n${choice.reasoning}`,
        ...choice.toolCalls.map(
          (call) =>
            `Tool: ${call.name || call.id || "unknown"}\n${format(call.arguments)}`,
        ),
      ]
        .filter(Boolean)
        .join("\n\n"),
    )
    .filter(Boolean);
  const readable =
    [
      ...details,
      parsed.usage !== null && `Usage:\n${format(parsed.usage)}`,
      parsed.error !== null && `Error:\n${format(parsed.error)}`,
      ...parsed.unknown.map(
        (entry) => `Other response data:\n${format(entry)}`,
      ),
    ]
      .filter(Boolean)
      .join("\n\n") || (body ? format(body) : "Empty body");
  const incomplete = status === 499 || parsed.status === "incomplete";

  async function copy() {
    try {
      await navigator.clipboard.writeText(view === "raw" ? body : readable);
      setCopyStatus("Copied");
    } catch {
      setCopyStatus("Could not copy response");
    }
  }

  return (
    <div className="min-w-0">
      <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-medium">Response body</h3>
        <div className="flex items-center gap-1" aria-label="Response display">
          <Button
            type="button"
            size="sm"
            variant={view === "readable" ? "secondary" : "ghost"}
            aria-pressed={view === "readable"}
            onClick={() => {
              setView("readable");
              setCopyStatus("");
            }}
          >
            Readable
          </Button>
          <Button
            type="button"
            size="sm"
            variant={view === "raw" ? "secondary" : "ghost"}
            aria-pressed={view === "raw"}
            onClick={() => {
              setView("raw");
              setCopyStatus("");
            }}
          >
            Raw
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            onClick={() => void copy()}
          >
            Copy
          </Button>
        </div>
      </div>
      {copyStatus && (
        <p role="status" className="mb-2 text-xs text-muted-foreground">
          {copyStatus}
        </p>
      )}
      {(incomplete || parsed.status === "failed") && (
        <p role="status" className="mb-2 text-xs text-muted-foreground">
          {parsed.status === "failed"
            ? "The response reported a failure."
            : "The response may be incomplete or truncated."}
        </p>
      )}
      <div className="max-h-96 min-w-0 overflow-auto rounded-lg border bg-muted/50 p-4 text-xs leading-relaxed">
        {view === "raw" ? (
          <pre className="font-mono break-all whitespace-pre-wrap">
            {body || "Empty body"}
          </pre>
        ) : parsed.format === "unknown" ? (
          <pre className="font-mono break-all whitespace-pre-wrap">
            {format(body) || "Empty body"}
          </pre>
        ) : (
          <div className="space-y-4">
            {parsed.choices.map((choice) => (
              <section key={`${choice.index}-${choice.id ?? ""}`}>
                {choice.text && (
                  <>
                    <h4 className="mb-1 font-medium">
                      Assistant
                      {parsed.choices.length > 1 ? ` ${choice.index + 1}` : ""}
                    </h4>
                    <p className="break-words whitespace-pre-wrap">
                      {choice.text}
                    </p>
                  </>
                )}
                {choice.reasoning && (
                  <details className="mt-2">
                    <summary className="cursor-pointer font-medium">
                      Reasoning
                    </summary>
                    <pre className="mt-2 break-all whitespace-pre-wrap">
                      {choice.reasoning}
                    </pre>
                  </details>
                )}
                {choice.toolCalls.length > 0 && (
                  <details className="mt-2">
                    <summary className="cursor-pointer font-medium">
                      Tool calls ({choice.toolCalls.length})
                    </summary>
                    {choice.toolCalls.map((call) => (
                      <div
                        key={`${call.index}-${call.id ?? ""}`}
                        className="mt-2"
                      >
                        <p className="font-medium">
                          {call.name || call.id || "Unknown tool"}
                        </p>
                        <pre className="break-all whitespace-pre-wrap">
                          {format(call.arguments)}
                        </pre>
                      </div>
                    ))}
                  </details>
                )}
              </section>
            ))}
            {parsed.usage !== null && (
              <details>
                <summary className="cursor-pointer font-medium">Usage</summary>
                <pre className="mt-2 break-all whitespace-pre-wrap">
                  {format(parsed.usage)}
                </pre>
              </details>
            )}
            {parsed.error !== null && (
              <details open>
                <summary className="cursor-pointer font-medium">Error</summary>
                <pre className="mt-2 break-all whitespace-pre-wrap">
                  {format(parsed.error)}
                </pre>
              </details>
            )}
            {parsed.unknown.length > 0 && (
              <details>
                <summary className="cursor-pointer font-medium">
                  Other response data
                </summary>
                <pre className="mt-2 break-all whitespace-pre-wrap">
                  {parsed.unknown.map(format).join("\n\n")}
                </pre>
              </details>
            )}
            {!details.length &&
              parsed.usage === null &&
              parsed.error === null &&
              parsed.unknown.length === 0 && (
                <pre className="font-mono break-all whitespace-pre-wrap">
                  {format(body) || "Empty body"}
                </pre>
              )}
          </div>
        )}
      </div>
      <p className="mt-2 text-xs text-muted-foreground">
        Captured responses are limited to 256 KiB and may be incomplete.
      </p>
    </div>
  );
}
