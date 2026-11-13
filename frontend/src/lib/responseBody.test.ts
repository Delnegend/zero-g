import { test } from "node:test";
import assert from "node:assert/strict";
import { parseResponseBody } from "./responseBody";

const frame = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`;

test("reconstructs interleaved chat choices and tool call fragments", () => {
  const body =
    [
      {
        choices: [
          { index: 1, delta: { content: "Second" } },
          {
            index: 0,
            delta: {
              content: "Hello ",
              reasoning_content: "think ",
              tool_calls: [
                {
                  index: 1,
                  id: "call_b",
                  function: { name: "read", arguments: '{"p":' },
                },
                {
                  index: 0,
                  id: "call_a",
                  function: { name: "find", arguments: '{"q":' },
                },
              ],
            },
          },
        ],
      },
      {
        choices: [
          {
            index: 0,
            delta: {
              content: "world",
              reasoning_content: "first",
              tool_calls: [
                { index: 0, function: { arguments: '"x"}' } },
                { index: 1, function: { arguments: '"y"}' } },
              ],
            },
            finish_reason: "tool_calls",
          },
        ],
      },
      { choices: [], usage: { prompt_tokens: 4, completion_tokens: 7 } },
    ]
      .map(frame)
      .join("") + "data: [DONE]\n\n";
  const parsed = parseResponseBody(body);
  assert.equal(parsed.format, "sse");
  assert.equal(parsed.status, "complete");
  assert.deepEqual(parsed.usage, { prompt_tokens: 4, completion_tokens: 7 });
  assert.deepEqual(parsed.choices, [
    {
      index: 0,
      id: null,
      text: "Hello world",
      reasoning: "think first",
      finishReason: "tool_calls",
      toolCalls: [
        { index: 0, id: "call_a", name: "find", arguments: '{"q":"x"}' },
        { index: 1, id: "call_b", name: "read", arguments: '{"p":"y"}' },
      ],
    },
    {
      index: 1,
      id: null,
      text: "Second",
      reasoning: "",
      finishReason: null,
      toolCalls: [],
    },
  ]);
});

test("keeps complete fragments from a truncated or malformed stream", () => {
  const prefix = frame({
    choices: [{ index: 0, delta: { content: "partial" } }],
  });
  for (const suffix of ["", 'data: {"choices":', 'data: {"choices":\n\n']) {
    const parsed = parseResponseBody(prefix + suffix);
    assert.equal(parsed.status, "incomplete");
    assert.equal(parsed.choices[0].text, "partial");
    if (suffix) assert.equal(parsed.unknown.length, 1);
  }
  assert.equal(
    parseResponseBody("data: [DONE]\n\ndata: {broken\n\n").status,
    "incomplete",
  );
});

test("parses SSE multiline data, CRLF and event names", () => {
  const body =
    ': heartbeat\r\nevent: response.output_text.delta\r\ndata: {"output_index":0,\r\ndata: "delta":"hello"}\r\n\r\n' +
    'event: response.completed\r\ndata: {"response":{"status":"completed"}}\r\n\r\n';
  const parsed = parseResponseBody(body);
  assert.equal(parsed.status, "complete");
  assert.equal(parsed.choices[0].text, "hello");
});

test("merges Responses API deltas and snapshots without duplicating terminal data", () => {
  const events = [
    {
      type: "response.output_item.added",
      output_index: 0,
      item: { type: "message", id: "msg", content: [] },
    },
    {
      type: "response.output_text.delta",
      item_id: "msg",
      output_index: 0,
      content_index: 0,
      delta: "Hi ",
    },
    {
      type: "response.output_text.delta",
      item_id: "msg",
      output_index: 0,
      content_index: 0,
      delta: "there",
    },
    {
      type: "response.reasoning_text.delta",
      item_id: "msg",
      output_index: 0,
      delta: "thought",
    },
    {
      type: "response.output_text.done",
      item_id: "msg",
      output_index: 0,
      text: "Hi there",
    },
    {
      type: "response.output_item.added",
      output_index: 1,
      item: { type: "function_call", id: "fc1", call_id: "c1", name: "lookup" },
    },
    {
      type: "response.output_item.added",
      output_index: 2,
      item: { type: "function_call", id: "fc2", call_id: "c2", name: "read" },
    },
    {
      type: "response.function_call_arguments.delta",
      item_id: "fc1",
      delta: '{"a":',
    },
    {
      type: "response.function_call_arguments.delta",
      item_id: "fc2",
      delta: '{"b":2}',
    },
    {
      type: "response.function_call_arguments.done",
      item_id: "fc1",
      arguments: '{"a":1}',
    },
    {
      type: "response.output_item.done",
      output_index: 1,
      item: {
        type: "function_call",
        id: "fc1",
        call_id: "c1",
        name: "lookup",
        arguments: '{"a":1}',
      },
    },
    {
      type: "response.completed",
      response: {
        status: "completed",
        usage: { input_tokens: 3 },
        output: [
          {
            type: "message",
            id: "msg",
            content: [{ type: "output_text", text: "Hi there" }],
          },
          {
            type: "function_call",
            id: "fc1",
            call_id: "c1",
            name: "lookup",
            arguments: '{"a":1}',
          },
          {
            type: "function_call",
            id: "fc2",
            call_id: "c2",
            name: "read",
            arguments: '{"b":2}',
          },
        ],
      },
    },
  ];
  const parsed = parseResponseBody(
    events.map(frame).join("") + frame(events.at(-1)),
  );
  assert.equal(parsed.status, "complete");
  assert.equal(parsed.choices[0].text, "Hi there");
  assert.equal(parsed.choices[0].reasoning, "thought");
  assert.deepEqual(parsed.choices[1].toolCalls, [
    { index: 1, id: "c1", name: "lookup", arguments: '{"a":1}' },
  ]);
  assert.deepEqual(parsed.choices[2].toolCalls, [
    { index: 2, id: "c2", name: "read", arguments: '{"b":2}' },
  ]);
  assert.deepEqual(parsed.usage, { input_tokens: 3 });
});

test("parses JSON responses and reports only reliable completion", () => {
  const json = {
    choices: [
      {
        index: 2,
        message: {
          content: "answer",
          reasoning_content: "why",
          tool_calls: [
            {
              id: "c",
              type: "function",
              function: { name: "search", arguments: "{}" },
            },
          ],
        },
      },
    ],
    usage: { total_tokens: 10 },
  };
  const before = JSON.stringify(json);
  const parsed = parseResponseBody(json);
  assert.equal(parsed.format, "json");
  assert.equal(parsed.status, "unknown");
  assert.equal(parsed.choices[0].text, "answer");
  assert.equal(parsed.choices[0].reasoning, "why");
  assert.equal(parsed.choices[0].toolCalls[0].name, "search");
  assert.equal(JSON.stringify(json), before);
  assert.equal(
    parseResponseBody({
      choices: [{ message: { content: "x" }, finish_reason: "stop" }],
    }).status,
    "complete",
  );
  assert.equal(
    parseResponseBody({
      choices: [{ message: { content: "x" }, finish_reason: "length" }],
    }).status,
    "incomplete",
  );
  const response = parseResponseBody(
    JSON.stringify({
      object: "response",
      status: "completed",
      output: [
        {
          type: "reasoning",
          id: "r",
          summary: [{ type: "summary_text", text: "Thinking" }],
        },
        {
          type: "message",
          id: "m",
          content: [{ type: "output_text", text: "Done" }],
        },
      ],
    }),
  );
  assert.equal(response.status, "complete");
  assert.equal(response.choices[0].reasoning, "Thinking");
  assert.equal(response.choices[1].text, "Done");
});

test("reports failures and retains unknown events and unrecognized input", () => {
  const mystery = {
    type: "response.future",
    value: "<img src=x onerror=alert(1)>",
  };
  const failed = {
    type: "response.failed",
    response: { status: "failed", error: { message: "broken" } },
  };
  const parsed = parseResponseBody(
    frame(mystery) + frame(failed) + frame(failed),
  );
  assert.equal(parsed.status, "failed");
  assert.deepEqual(parsed.error, { message: "broken" });
  assert.deepEqual(parsed.unknown, [mystery]);
  const unrecognized = parseResponseBody("plain response");
  assert.equal(unrecognized.format, "unknown");
  assert.deepEqual(unrecognized.unknown, ["plain response"]);
  assert.equal(
    parseResponseBody(
      frame({
        type: "response.incomplete",
        response: { status: "incomplete" },
      }),
    ).status,
    "incomplete",
  );
});

test("reconstructs Anthropic Messages streams with thinking, tools, and usage", () => {
  const events = [
    {
      type: "message_start",
      message: {
        id: "msg_1",
        model: "claude-opus-5-5",
        content: [],
        usage: { input_tokens: 10, output_tokens: 1 },
      },
    },
    {
      type: "content_block_start",
      index: 0,
      content_block: { type: "thinking", thinking: "" },
    },
    {
      type: "content_block_delta",
      index: 0,
      delta: { type: "thinking_delta", thinking: "Check 7." },
    },
    {
      type: "content_block_delta",
      index: 0,
      delta: { type: "signature_delta", signature: "sig" },
    },
    { type: "content_block_stop", index: 0 },
    {
      type: "content_block_start",
      index: 1,
      content_block: { type: "text", text: "" },
    },
    {
      type: "content_block_delta",
      index: 1,
      delta: { type: "text_delta", text: "Not prime." },
    },
    {
      type: "content_block_start",
      index: 2,
      content_block: { type: "tool_use", id: "tu", name: "calc", input: {} },
    },
    {
      type: "content_block_delta",
      index: 2,
      delta: { type: "input_json_delta", partial_json: '{"n":' },
    },
    {
      type: "content_block_delta",
      index: 2,
      delta: { type: "input_json_delta", partial_json: "1001}" },
    },
    { type: "ping" },
    {
      type: "message_delta",
      delta: { stop_reason: "tool_use" },
      usage: { output_tokens: 42 },
    },
    { type: "message_stop" },
  ];
  const parsed = parseResponseBody(events.map(frame).join(""));
  assert.equal(parsed.format, "sse");
  assert.equal(parsed.status, "complete");
  assert.deepEqual(parsed.metadata, { id: "msg_1", model: "claude-opus-5-5" });
  assert.deepEqual(parsed.usage, { input_tokens: 10, output_tokens: 42 });
  assert.deepEqual(parsed.unknown, []);
  assert.equal(parsed.choices[0].reasoning, "Check 7.");
  assert.equal(parsed.choices[0].text, "Not prime.");
  assert.equal(parsed.choices[0].finishReason, "tool_use");
  assert.deepEqual(parsed.choices[0].toolCalls, [
    { index: 2, id: "tu", name: "calc", arguments: '{"n":1001}' },
  ]);
});

test("keeps chat stream identity and Responses lifecycle events out of unknown data", () => {
  const chat = parseResponseBody(
    frame({
      id: "chatcmpl-1",
      model: "m",
      created: 5,
      object: "chat.completion.chunk",
      choices: [{ index: 0, delta: { role: "assistant" } }],
    }) + "data: [DONE]\n\n",
  );
  assert.deepEqual(chat.metadata, { id: "chatcmpl-1", model: "m", created: 5 });
  const responses = parseResponseBody(
    [
      {
        type: "response.created",
        response: { id: "resp", model: "m", status: "in_progress" },
      },
      {
        type: "response.content_part.done",
        item_id: "msg",
        output_index: 0,
        content_index: 0,
        part: { type: "output_text", text: "Hi" },
      },
      { type: "response.completed", response: { status: "completed" } },
    ]
      .map(frame)
      .join(""),
  );
  assert.deepEqual(responses.unknown, []);
  assert.deepEqual(responses.metadata, { id: "resp", model: "m" });
  assert.equal(responses.choices[0].text, "Hi");
});
