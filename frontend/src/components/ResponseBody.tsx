import { useMemo, useState } from "react";
import { Button } from "@/components/ui/button";
import { JsonCode, JsonTokens, toJsonText } from "@/components/HighlightedJson";
import { parseResponseBody } from "@/lib/responseBody";

const format = (value: unknown): string => toJsonText(value).text;

/** Collapsible block with an 8px gap between the toggle and its content. */
function Section({
  title,
  open,
  children,
}: {
  title: string;
  open?: boolean;
  children: React.ReactNode;
}) {
  return (
    <details open={open} className="group">
      <summary className="cursor-pointer font-medium">{title}</summary>
      <div className="mt-2 space-y-2">{children}</div>
    </details>
  );
}

function Code({ value }: { value: unknown }) {
  return (
    <pre className="break-all whitespace-pre-wrap">
      <JsonCode value={value} />
    </pre>
  );
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
  const metadata = {
    ...parsed.metadata,
    ...Object.fromEntries(
      parsed.choices
        .filter((choice) => choice.finishReason !== null)
        .map((choice) => [
          parsed.choices.length > 1
            ? `finish_reason_${choice.index + 1}`
            : "finish_reason",
          choice.finishReason,
        ]),
    ),
  };
  const hasMetadata = Object.keys(metadata).length > 0;
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
      hasMetadata && `Response:\n${format(metadata)}`,
      parsed.usage !== null && `Usage:\n${format(parsed.usage)}`,
      parsed.error !== null && `Error:\n${format(parsed.error)}`,
      ...parsed.unknown.map(
        (entry) => `Other response data:\n${format(entry)}`,
      ),
    ]
      .filter(Boolean)
      .join("\n\n") || (body ? format(body) : "Empty body");
  const incomplete = status === 499 || parsed.status === "incomplete";
  const choices = parsed.choices.filter(
    (choice) => choice.text || choice.reasoning || choice.toolCalls.length,
  );

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
      <div className="json-code max-h-96 min-w-0 overflow-auto rounded-lg border bg-muted/50 p-4 text-xs leading-relaxed">
        {view === "raw" ? (
          <pre className="font-mono break-all whitespace-pre-wrap">
            {body ? <JsonTokens text={body} /> : "Empty body"}
          </pre>
        ) : parsed.format === "unknown" ? (
          <pre className="font-mono break-all whitespace-pre-wrap">
            {body ? <JsonCode value={body} /> : "Empty body"}
          </pre>
        ) : (
          <div className="space-y-4">
            {choices.map((choice) => (
              <section
                key={`${choice.index}-${choice.id ?? ""}`}
                className="space-y-2"
              >
                {choice.text && (
                  <div>
                    <h4 className="mb-1 font-medium">
                      Assistant
                      {parsed.choices.length > 1 ? ` ${choice.index + 1}` : ""}
                    </h4>
                    <p className="wrap-break-word whitespace-pre-wrap">
                      {choice.text}
                    </p>
                  </div>
                )}
                {choice.reasoning && (
                  <Section title="Reasoning">
                    <pre className="break-all whitespace-pre-wrap">
                      {choice.reasoning}
                    </pre>
                  </Section>
                )}
                {choice.toolCalls.length > 0 && (
                  <Section title={`Tool calls (${choice.toolCalls.length})`}>
                    {choice.toolCalls.map((call) => (
                      <div key={`${call.index}-${call.id ?? ""}`}>
                        <p className="font-medium">
                          {call.name || call.id || "Unknown tool"}
                        </p>
                        <Code value={call.arguments} />
                      </div>
                    ))}
                  </Section>
                )}
              </section>
            ))}
            {hasMetadata && (
              <Section title="Response">
                <Code value={metadata} />
              </Section>
            )}
            {parsed.usage !== null && (
              <Section title="Usage">
                <Code value={parsed.usage} />
              </Section>
            )}
            {parsed.error !== null && (
              <Section title="Error" open>
                <Code value={parsed.error} />
              </Section>
            )}
            {parsed.unknown.length > 0 && (
              <Section title="Other response data">
                {parsed.unknown.map((entry, i) => (
                  <Code key={i} value={entry} />
                ))}
              </Section>
            )}
            {!details.length &&
              !hasMetadata &&
              parsed.usage === null &&
              parsed.error === null &&
              parsed.unknown.length === 0 && (
                <pre className="font-mono break-all whitespace-pre-wrap">
                  {body ? <JsonCode value={body} /> : "Empty body"}
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
