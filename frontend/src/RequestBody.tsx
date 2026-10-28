import { useMemo } from "react";
import { createHighlighter } from "@tanstack/highlight/core";
import { json } from "@tanstack/highlight/languages/json";
import { createThemeCss } from "@tanstack/highlight/theme";
import { githubDarkTheme } from "@tanstack/highlight/themes/github-dark";
import { githubLightTheme } from "@tanstack/highlight/themes/github-light";

const highlighter = createHighlighter({ languages: [json] });
export const requestBodyTheme = createThemeCss({
  light: githubLightTheme,
  dark: githubDarkTheme,
  lightSelector: ".request-body",
  darkSelector: ".dark .request-body",
  includeBaseStyles: false,
});

export function RequestBody({ body }: { body: unknown }) {
  const content = useMemo(() => {
    if (body == null) return { text: "No body recorded", tokens: null };
    try {
      const value = typeof body === "string" ? JSON.parse(body) : body;
      const text = JSON.stringify(value, null, 2);
      if (text === undefined) return { text: "No body recorded", tokens: null };
      return {
        text,
        tokens: highlighter.tokenize(text, { lang: "json" }).tokens,
      };
    } catch {
      return {
        text:
          typeof body === "string" ? body || "Empty body" : "No body recorded",
        tokens: null,
      };
    }
  }, [body]);

  return (
    <pre className="request-body max-h-96 overflow-auto rounded-lg border bg-muted/50 p-4 text-xs leading-relaxed">
      <code>
        {content.tokens
          ? content.tokens.map((token, index) => (
              <span
                key={index}
                data-token={token.className}
                style={
                  token.className
                    ? { color: `var(--th-${token.className})` }
                    : undefined
                }
              >
                {token.value}
              </span>
            ))
          : content.text}
      </code>
    </pre>
  );
}
