import { useMemo } from "react";
import { JsonTokens, toJsonText } from "@/components/HighlightedJson";

export function RequestBody({ body }: { body: unknown }) {
  const content = useMemo(() => {
    if (body == null) return { text: "No body recorded", json: false };
    const formatted = toJsonText(body);
    if (formatted.json) return formatted;
    return {
      text:
        typeof body === "string" ? body || "Empty body" : "No body recorded",
      json: false,
    };
  }, [body]);

  return (
    <pre className="json-code max-h-96 overflow-auto rounded-lg border bg-muted/50 p-4 text-xs leading-relaxed text-wrap">
      <code>
        {content.json ? <JsonTokens text={content.text} /> : content.text}
      </code>
    </pre>
  );
}
