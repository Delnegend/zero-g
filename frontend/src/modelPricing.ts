// Antigravity's subscription is not billed at these API rates. Match only
// explicit model IDs, Claude's thinking variant, or a declared base model.
export function antigravityReferencePrice(
  sheet: Record<string, unknown> | undefined,
  id: string,
): Record<string, unknown> | undefined {
  if (!sheet) return undefined;
  const claudeId = id.startsWith("claude-") ? id.replace(/-thinking$/, "") : id;
  const candidates = id.startsWith("gemini-")
    ? [
        [`gemini/${id}`, "gemini", false],
        [`vertex_ai/${id}`, "vertex_ai", false],
        [`gemini/${id}-preview`, "gemini", true],
        [`vertex_ai/${id}-preview`, "vertex_ai", true],
      ]
    : id.startsWith("claude-")
      ? [
          [claudeId, "anthropic", false],
          [`anthropic/${claudeId}`, "anthropic", false],
        ]
      : [];
  for (const [key, owner, alias] of candidates) {
    const entry = sheet[key as string];
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) continue;
    const price = entry as Record<string, unknown>;
    if (price.provider !== owner || (alias && price.base_model !== id))
      continue;
    return price;
  }
  return undefined;
}
