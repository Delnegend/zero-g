import { test } from "node:test";
import assert from "node:assert/strict";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { ResponseBody } from "./ResponseBody";

test("shows reconstructed response, raw stream, and copies the selected view", async () => {
  const raw =
    'data: {"choices":[{"index":0,"delta":{"content":"Hello "}}]}\n\ndata: {"choices":[{"index":0,"delta":{"content":"world"},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n';
  let copied = "";
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: {
      writeText: async (text: string) => {
        copied = text;
      },
    },
  });
  try {
    const view = render(<ResponseBody body={raw} status={200} />);
    assert.ok(screen.getByText("Hello world"));
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await screen.findByText("Copied");
    assert.equal(copied, "Hello world");
    fireEvent.click(screen.getByRole("button", { name: "Raw" }));
    assert.equal(view.container.querySelector("pre")?.textContent, raw);
    fireEvent.click(screen.getByRole("button", { name: "Copy" }));
    await screen.findByText("Copied");
    assert.equal(copied, raw);
  } finally {
    cleanup();
  }
});

test("incomplete responses show a warning and captured content safely", () => {
  try {
    const raw =
      'data: {"choices":[{"index":0,"delta":{"content":"<script>alert(1)</script>"}}]}\n\n';
    const view = render(<ResponseBody body={raw} status={499} />);
    assert.ok(screen.getByText(/may be incomplete or truncated/));
    assert.ok(screen.getByText("<script>alert(1)</script>"));
    assert.equal(view.container.querySelector("script"), null);
  } finally {
    cleanup();
  }
});
