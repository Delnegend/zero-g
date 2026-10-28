import { test } from "node:test";
import assert from "node:assert/strict";
import { renderToStaticMarkup } from "react-dom/server";
import { RequestBody } from "./RequestBody";

test("highlights JSON without interpreting its contents as HTML", () => {
  const html = renderToStaticMarkup(
    <RequestBody
      body={'{"message":"<script>alert(1)</script>","count":42,"ok":true}'}
    />,
  );
  assert.match(html, /data-token="property"/);
  assert.match(html, /data-token="number"/);
  assert.ok(html.includes("&lt;script&gt;alert(1)&lt;/script&gt;"));
  assert.ok(!html.includes("<script>"));
});

test("keeps streamed or non-JSON responses as plain text", () => {
  const body = 'data: {"message":"hello"}\n\ndata: [DONE]';
  const html = renderToStaticMarkup(<RequestBody body={body} />);
  assert.ok(html.includes("data: "));
  assert.ok(html.includes("[DONE]"));
  assert.ok(!html.includes("data-token="));
  assert.ok(
    renderToStaticMarkup(<RequestBody body={null} />).includes(
      "No body recorded",
    ),
  );
});
