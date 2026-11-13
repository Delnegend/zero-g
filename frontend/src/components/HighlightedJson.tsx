import { createHighlighter } from "@tanstack/highlight/core";
import { json } from "@tanstack/highlight/languages/json";
import { createThemeCss } from "@tanstack/highlight/theme";
import { githubDarkTheme } from "@tanstack/highlight/themes/github-dark";
import { githubLightTheme } from "@tanstack/highlight/themes/github-light";
import { useMemo } from "react";

const highlighter = createHighlighter({ languages: [json] });

/** Token colors for any element inside a `.json-code` container. */
export const jsonTheme = createThemeCss({
  light: githubLightTheme,
  dark: githubDarkTheme,
  lightSelector: ".json-code",
  darkSelector: ".dark .json-code",
  includeBaseStyles: false,
});

/** Pretty-printed JSON when `value` is or contains JSON, otherwise the text unchanged. */
export function toJsonText(value: unknown): { text: string; json: boolean } {
  if (typeof value === "string") {
    try {
      return { text: JSON.stringify(JSON.parse(value), null, 2), json: true };
    } catch {
      return { text: value, json: false };
    }
  }
  const text = JSON.stringify(value, null, 2);
  return text === undefined ? { text: "", json: false } : { text, json: true };
}

/** Colored spans for JSON source; the rendered text equals `text`. */
export function JsonTokens({ text }: { text: string }) {
  const tokens = useMemo(
    () => highlighter.tokenize(text, { lang: "json" }).tokens,
    [text],
  );
  return tokens.map((token, index) => (
    <span
      key={index}
      data-token={token.className}
      style={
        token.className ? { color: `var(--th-${token.className})` } : undefined
      }
    >
      {token.value}
    </span>
  ));
}

/** Highlighted, pretty-printed JSON, or plain text when `value` is not JSON. */
export function JsonCode({ value }: { value: unknown }) {
  const { text, json } = useMemo(() => toJsonText(value), [value]);
  return json ? <JsonTokens text={text} /> : text;
}
