import { test } from "node:test";
import assert from "node:assert/strict";
import { antigravityReferencePrice } from "./modelPricing";

test("uses matching Gemini and Claude API prices as references", () => {
  const gemini = {
    provider: "gemini",
    base_model: "gemini-3-pro",
    input_cost_per_token: 0.000002,
  };
  const claude = { provider: "anthropic", input_cost_per_token: 0.000003 };
  const sheet = {
    "gemini/gemini-3-pro-preview": gemini,
    "claude-sonnet-4-6": claude,
  };
  assert.equal(antigravityReferencePrice(sheet, "gemini-3-pro"), gemini);
  assert.equal(antigravityReferencePrice(sheet, "claude-sonnet-4-6"), claude);
  assert.equal(
    antigravityReferencePrice(sheet, "claude-sonnet-4-6-thinking"),
    claude,
  );
  assert.equal(antigravityReferencePrice(sheet, "gemini-3-flash"), undefined);
});

test("does not use another provider or an unconfirmed preview alias", () => {
  assert.equal(
    antigravityReferencePrice(
      { "claude-sonnet-4-6": { provider: "bedrock" } },
      "claude-sonnet-4-6",
    ),
    undefined,
  );
  assert.equal(
    antigravityReferencePrice(
      {
        "gemini/gemini-3-pro-preview": {
          provider: "gemini",
          base_model: "different",
        },
      },
      "gemini-3-pro",
    ),
    undefined,
  );
});
