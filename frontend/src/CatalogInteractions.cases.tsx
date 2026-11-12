import { test } from "node:test";
import assert from "node:assert/strict";
import {
  within,
  screen,
  userEvent,
  ModelCatalogPage,
  TooltipProvider,
  mount,
} from "./interactionTestSetup";

test("catalog distinguishes Decisions models without guessing missing metadata", async () => {
  const card = (id: string, model: Record<string, unknown>) => ({
    model: { id, ...model },
    display_name: id,
    description: null,
    reasoning_efforts: [],
    reasoning: null,
    tool_call: null,
    input_modalities: ["text"],
    pricing: null,
    raw_pricing: null,
    parameters: null,
    provider_metadata: null,
  });
  globalThis.fetch = (async () =>
    Response.json({
      datasheets_error: null,
      providers: [
        {
          id: "openrouter",
          name: "OpenRouter",
          kind: "openrouter",
          base_url: "",
          error: null,
          models: [
            card("decision-only", {
              architecture: { output_modalities: ["decisions"] },
              endpoints: ["/api/alpha/decisions"],
            }),
            card("mixed", {
              architecture: { output_modalities: ["text", "decisions"] },
              endpoints: ["/v1/chat/completions", "/api/alpha/decisions"],
            }),
            card("ordinary-chat", {
              architecture: { output_modalities: ["text"] },
              endpoints: ["/v1/chat/completions"],
            }),
            card("unknown", {
              architecture: { output_modalities: "decisions" },
            }),
          ],
        },
      ],
    })) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <ModelCatalogPage />
    </TooltipProvider>,
  );
  const modelCard = async (name: string) => {
    const heading = await screen.findByRole("heading", { name });
    const element = heading.closest('[data-slot="card"]');
    assert.ok(element instanceof HTMLElement);
    return within(element);
  };
  assert.ok(
    (await modelCard("decision-only")).getByText(
      "Decisions API only (not Chat Completions or Responses)",
    ),
  );
  assert.ok(
    (await modelCard("mixed")).getByText(
      "Decisions API and other capabilities",
    ),
  );
  assert.equal(
    (await modelCard("ordinary-chat")).queryByText(/Decisions API/),
    null,
  );
  assert.equal((await modelCard("unknown")).queryByText(/Decisions API/), null);
  await user.type(
    screen.getByLabelText("Search models or providers"),
    "decision-only",
  );
  assert.ok(await screen.findByRole("heading", { name: "decision-only" }));
  assert.equal(screen.queryByRole("heading", { name: "ordinary-chat" }), null);
});

test("model cards label supported input icons and retain unknown modalities", async () => {
  globalThis.fetch = (async () =>
    Response.json({
      datasheets_error: null,
      providers: [
        {
          id: "example",
          name: "Example",
          kind: "agy",
          base_url: "",
          error: null,
          models: [
            {
              model: { id: "model" },
              display_name: "Model",
              description: null,
              reasoning_efforts: [],
              reasoning: null,
              tool_call: null,
              input_modalities: [
                "text",
                "image",
                "audio",
                "video",
                "pdf",
                "file",
                "geometry",
                "toString",
              ],
              pricing: null,
              raw_pricing: null,
              parameters: null,
              provider_metadata: null,
            },
          ],
        },
        {
          id: "empty",
          name: "Empty provider",
          kind: "agy",
          base_url: "",
          error: null,
          models: [],
        },
      ],
    })) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <ModelCatalogPage />
    </TooltipProvider>,
  );
  assert.ok(await screen.findByRole("heading", { name: "Example 1 model" }));
  assert.ok(screen.getByRole("heading", { name: "Empty provider" }));
  for (const modality of ["text", "image", "audio", "video", "pdf", "file"]) {
    assert.ok(await screen.findByRole("button", { name: `${modality} input` }));
  }
  assert.ok(screen.getByText("geometry"));
  assert.ok(screen.getByText("toString"));
  await user.click(
    screen.getByRole("button", { name: "About Antigravity input support" }),
  );
  assert.ok(
    await screen.findByText(/forwards inline audio, video, and PDF data/i),
  );
});

test("catalog explains subscription reference pricing and Claude PDF limitations", async () => {
  globalThis.fetch = (async () =>
    Response.json({
      datasheets_error: null,
      providers: [
        {
          id: "claude",
          name: "Claude Code",
          kind: "claude",
          base_url: "",
          error: null,
          models: [
            {
              model: { id: "claude-example", context_window: 200_000 },
              display_name: "Claude Example",
              description: null,
              reasoning_efforts: [],
              reasoning: true,
              tool_call: true,
              input_modalities: ["text", "image", "pdf"],
              pricing: {
                input: 1,
                output: 5,
                cache_write: null,
                cache_read: null,
                source: "claude-reference",
              },
              raw_pricing: null,
              parameters: null,
              provider_metadata: null,
            },
          ],
        },
        {
          id: "codex",
          name: "OpenAI Codex",
          kind: "codex",
          base_url: "",
          error: null,
          models: [
            {
              model: { id: "gpt-5.3-codex" },
              display_name: "GPT-5.3-Codex",
              description: null,
              reasoning_efforts: [],
              reasoning: null,
              tool_call: null,
              input_modalities: ["text", "image"],
              pricing: {
                input: 1.75,
                output: 14,
                cache_write: null,
                cache_read: 0.175,
                source: "codex-reference",
              },
              raw_pricing: null,
              parameters: null,
              provider_metadata: null,
            },
          ],
        },
      ],
    })) as typeof fetch;
  const user = userEvent.setup();
  mount(
    <TooltipProvider>
      <ModelCatalogPage />
    </TooltipProvider>,
  );
  assert.ok(await screen.findByText("200,000"));
  assert.ok(screen.getByText("$1"));
  const claudeCard = screen
    .getByRole("heading", { name: "Claude Example" })
    .closest('[data-slot="card"]');
  assert.ok(claudeCard);
  await user.click(
    within(claudeCard as HTMLElement).getByRole("button", {
      name: "About model pricing",
    }),
  );
  assert.ok(await screen.findByText(/not Claude subscription charges/i));
  await user.keyboard("{Escape}");
  const codexCard = screen
    .getByRole("heading", { name: "GPT-5.3-Codex" })
    .closest('[data-slot="card"]');
  assert.ok(codexCard);
  await user.click(
    within(codexCard as HTMLElement).getByRole("button", {
      name: "About model pricing",
    }),
  );
  assert.ok(await screen.findByText(/not Codex subscription charges/i));
  await user.keyboard("{Escape}");
  await user.click(
    screen.getByRole("button", { name: "About Claude input support" }),
  );
  assert.ok(await screen.findByText(/only text and images to Claude/i));
});
