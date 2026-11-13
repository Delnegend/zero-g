export type ResponseStatus = "complete" | "incomplete" | "failed" | "unknown";

export interface ResponseToolCall {
  index: number;
  id: string | null;
  name: string;
  arguments: string;
}

export interface ResponseChoice {
  index: number;
  id: string | null;
  text: string;
  reasoning: string;
  toolCalls: ResponseToolCall[];
  finishReason: string | null;
}

export interface ParsedResponseBody {
  format: "json" | "sse" | "unknown";
  status: ResponseStatus;
  /** Response identity (`id`, `model`, `created`, ...) as first reported. */
  metadata: Record<string, string | number>;
  choices: ResponseChoice[];
  usage: unknown | null;
  error: unknown | null;
  unknown: unknown[];
}

type ObjectValue = Record<string, unknown>;
type ChoiceState = ResponseChoice & {
  textParts: Map<number, string>;
  reasoningParts: Map<number, string>;
};

const object = (value: unknown): ObjectValue | null =>
  value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as ObjectValue)
    : null;
const string = (value: unknown): string | null =>
  typeof value === "string" ? value : null;
const index = (value: unknown, fallback: number): number =>
  typeof value === "number" && Number.isInteger(value) && value >= 0
    ? value
    : fallback;

/** Parse recorded response data without modifying the input or interpreting it as HTML. */
export function parseResponseBody(body: unknown): ParsedResponseBody {
  const result: ParsedResponseBody = {
    format: "unknown",
    status: "unknown",
    metadata: {},
    choices: [],
    usage: null,
    error: null,
    unknown: [],
  };
  const choices = new Map<number, ChoiceState>();
  const itemIndexes = new Map<string, number>();
  let recognized = false;
  let terminal = false;
  let malformed = false;
  const meta = (value: ObjectValue | null) => {
    if (!value) return;
    for (const key of [
      "id",
      "model",
      "created",
      "created_at",
      "system_fingerprint",
      "service_tier",
    ]) {
      const field = value[key];
      if (
        result.metadata[key] === undefined &&
        (typeof field === "string" || typeof field === "number") &&
        field !== ""
      )
        result.metadata[key] = field;
    }
  };
  /** Merge usage objects, as Anthropic splits counts across stream events. */
  const usage = (value: unknown) => {
    if (value == null) return;
    const current = object(result.usage);
    const next = object(value);
    result.usage = current && next ? { ...current, ...next } : value;
  };

  const setStatus = (status: ResponseStatus) => {
    const rank: Record<ResponseStatus, number> = {
      unknown: 0,
      complete: 1,
      incomplete: 2,
      failed: 3,
    };
    if (rank[status] > rank[result.status]) result.status = status;
  };
  const choice = (number: number): ChoiceState => {
    let state = choices.get(number);
    if (!state) {
      state = {
        index: number,
        id: null,
        text: "",
        reasoning: "",
        toolCalls: [],
        finishReason: null,
        textParts: new Map(),
        reasoningParts: new Map(),
      };
      choices.set(number, state);
    }
    return state;
  };
  const tool = (state: ChoiceState, number: number): ResponseToolCall => {
    let call = state.toolCalls.find((item) => item.index === number);
    if (!call) {
      call = { index: number, id: null, name: "", arguments: "" };
      state.toolCalls.push(call);
    }
    return call;
  };
  const part = (
    state: ChoiceState,
    kind: "text" | "reasoning",
    number: number,
    value: string,
    append: boolean,
  ) => {
    const parts = kind === "text" ? state.textParts : state.reasoningParts;
    parts.set(number, (append ? (parts.get(number) ?? "") : "") + value);
  };
  const content = (
    state: ChoiceState,
    value: unknown,
    append: boolean,
    reasoning = false,
  ) => {
    if (typeof value === "string") {
      part(state, reasoning ? "reasoning" : "text", 0, value, append);
    } else if (Array.isArray(value)) {
      value.forEach((entry, i) => {
        const item = object(entry);
        if (!item) {
          result.unknown.push(entry);
          return;
        }
        const text = string(item.text) ?? string(item.refusal);
        if (
          text !== null &&
          (item.type === "text" ||
            item.type === "output_text" ||
            item.type === "reasoning_text" ||
            item.type === "summary_text" ||
            item.type === "refusal")
        ) {
          part(
            state,
            reasoning ||
              item.type === "reasoning_text" ||
              item.type === "summary_text"
              ? "reasoning"
              : "text",
            i,
            text,
            append,
          );
        } else result.unknown.push(entry);
      });
    } else if (value != null) result.unknown.push(value);
  };
  const statusFrom = (value: unknown) => {
    if (value === "completed" || value === "complete") {
      terminal = true;
      setStatus("complete");
    } else if (value === "incomplete") {
      terminal = true;
      setStatus("incomplete");
    } else if (
      value === "failed" ||
      value === "cancelled" ||
      value === "canceled"
    ) {
      terminal = true;
      setStatus("failed");
    }
  };
  const itemIndex = (value: ObjectValue, item?: ObjectValue | null): number => {
    const id = string(value.item_id) ?? string(item?.id);
    const existing = id === null ? undefined : itemIndexes.get(id);
    const number = index(value.output_index, existing ?? choices.size);
    if (id !== null) itemIndexes.set(id, number);
    return number;
  };
  const responseItem = (item: ObjectValue, number: number) => {
    const state = choice(number);
    const id = string(item.id);
    if (id !== null) {
      state.id = id;
      itemIndexes.set(id, number);
    }
    if (item.type === "message") {
      if (item.content !== undefined) content(state, item.content, false);
    } else if (item.type === "reasoning") {
      if (item.summary !== undefined) content(state, item.summary, false, true);
      if (item.content !== undefined) content(state, item.content, false, true);
    } else if (item.type === "function_call") {
      const call = tool(state, number);
      call.id = string(item.call_id) ?? call.id;
      call.name = string(item.name) ?? call.name;
      call.arguments = string(item.arguments) ?? call.arguments;
    } else result.unknown.push(item);
  };
  const response = (value: ObjectValue) => {
    usage(value.usage);
    meta(value);
    if (value.error != null) {
      result.error = value.error;
      setStatus("failed");
    }
    statusFrom(value.status);
    if (Array.isArray(value.output)) {
      value.output.forEach((entry, i) => {
        const item = object(entry);
        if (item) responseItem(item, i);
        else result.unknown.push(entry);
      });
    } else if (typeof value.output_text === "string") {
      part(choice(0), "text", 0, value.output_text, false);
    }
  };
  const chat = (value: ObjectValue) => {
    if (!Array.isArray(value.choices)) return false;
    recognized = true;
    usage(value.usage);
    meta(value);
    let allFinished = value.choices.length > 0;
    value.choices.forEach((entry, i) => {
      const item = object(entry);
      if (!item) {
        result.unknown.push(entry);
        allFinished = false;
        return;
      }
      const state = choice(index(item.index, i));
      const delta = object(item.delta);
      const message = delta ?? object(item.message);
      if (message) {
        if (message.content !== undefined)
          content(state, message.content, !!delta);
        if (message.reasoning_content !== undefined)
          content(state, message.reasoning_content, !!delta, true);
        else if (message.reasoning !== undefined)
          content(state, message.reasoning, !!delta, true);
        if (message.refusal != null) content(state, message.refusal, !!delta);
        if (Array.isArray(message.tool_calls))
          message.tool_calls.forEach((entry, j) => {
            const callValue = object(entry);
            if (!callValue) {
              result.unknown.push(entry);
              return;
            }
            const call = tool(state, index(callValue.index, j));
            const fn = object(callValue.function);
            call.id = string(callValue.id) ?? call.id;
            if (fn) {
              const name = string(fn.name);
              const args = string(fn.arguments);
              if (name !== null) call.name = delta ? call.name + name : name;
              if (args !== null)
                call.arguments = delta ? call.arguments + args : args;
            } else if (callValue.type && callValue.type !== "function")
              result.unknown.push(entry);
          });
      }
      const finish = string(item.finish_reason);
      if (finish !== null) {
        state.finishReason = finish;
        if (finish === "length" || finish === "content_filter")
          setStatus("incomplete");
      } else allFinished = false;
    });
    if (
      allFinished &&
      value.choices.every((entry) => object(entry)?.delta == null)
    )
      setStatus("complete");
    return true;
  };
  /** Anthropic Messages content block: text, thinking, or tool use. */
  const anthropicBlock = (block: ObjectValue, number: number) => {
    const state = choice(0);
    if (block.type === "text") {
      part(state, "text", number, string(block.text) ?? "", false);
    } else if (block.type === "thinking") {
      part(state, "reasoning", number, string(block.thinking) ?? "", false);
    } else if (block.type === "redacted_thinking") {
      part(state, "reasoning", number, "[redacted]", false);
    } else if (block.type === "tool_use" || block.type === "server_tool_use") {
      const call = tool(state, number);
      call.id = string(block.id) ?? call.id;
      call.name = string(block.name) ?? call.name;
      const input = object(block.input);
      if (input && Object.keys(input).length)
        call.arguments = JSON.stringify(input);
    } else result.unknown.push(block);
  };
  const stopReason = (value: unknown) => {
    const reason = string(value);
    if (reason === null) return;
    choice(0).finishReason = reason;
    if (reason === "max_tokens" || reason === "refusal")
      setStatus("incomplete");
  };
  /** Anthropic Messages JSON or stream event; false when `data` is not one. */
  const anthropic = (data: ObjectValue, type: string | undefined) => {
    if (type === "message") {
      meta(data);
      usage(data.usage);
      if (Array.isArray(data.content))
        data.content.forEach((entry, i) => {
          const block = object(entry);
          if (block) anthropicBlock(block, i);
          else result.unknown.push(entry);
        });
      stopReason(data.stop_reason);
      if (data.stop_reason != null) {
        terminal = true;
        setStatus("complete");
      }
      return true;
    }
    if (type === "message_start") {
      const message = object(data.message);
      meta(message);
      usage(message?.usage);
      return true;
    }
    if (type === "content_block_start") {
      const block = object(data.content_block);
      if (block) anthropicBlock(block, index(data.index, 0));
      return true;
    }
    if (type === "content_block_delta") {
      const delta = object(data.delta);
      const number = index(data.index, 0);
      const state = choice(0);
      if (delta?.type === "text_delta")
        part(state, "text", number, string(delta.text) ?? "", true);
      else if (delta?.type === "thinking_delta")
        part(state, "reasoning", number, string(delta.thinking) ?? "", true);
      else if (delta?.type === "input_json_delta") {
        const call = tool(state, number);
        call.arguments += string(delta.partial_json) ?? "";
      } else if (delta?.type !== "signature_delta") result.unknown.push(data);
      return true;
    }
    if (type === "message_delta") {
      usage(data.usage);
      stopReason(object(data.delta)?.stop_reason);
      return true;
    }
    if (type === "message_stop") {
      terminal = true;
      setStatus("complete");
      return true;
    }
    return type === "content_block_stop" || type === "ping";
  };
  const event = (value: unknown, name?: string) => {
    const data = object(value);
    if (!data) {
      result.unknown.push(value);
      return;
    }
    if (chat(data)) return;
    const type = string(data.type) ?? name;
    if (type === "error") {
      recognized = true;
      terminal = true;
      result.error = data.error ?? data;
      setStatus("failed");
      return;
    }
    if (anthropic(data, type)) {
      recognized = true;
      return;
    }
    if (
      type === "response.created" ||
      type === "response.in_progress" ||
      type === "response.queued"
    ) {
      recognized = true;
      const snapshot = object(data.response);
      if (snapshot) {
        meta(snapshot);
        usage(snapshot.usage);
      }
      return;
    }
    if (
      type === "response.content_part.added" ||
      type === "response.content_part.done"
    ) {
      // Text arrives through `output_text` deltas; a finished part restates it.
      recognized = true;
      const text = string(object(data.part)?.text);
      if (type.endsWith(".done") && text) {
        const state = choice(itemIndex(data));
        part(state, "text", index(data.content_index, 0), text, false);
      }
      return;
    }
    if (
      type === "response.completed" ||
      type === "response.done" ||
      type === "response.incomplete" ||
      type === "response.failed"
    ) {
      recognized = true;
      terminal = true;
      setStatus(
        type === "response.failed"
          ? "failed"
          : type === "response.incomplete"
            ? "incomplete"
            : "complete",
      );
      const snapshot = object(data.response);
      if (snapshot) response(snapshot);
      if (data.error != null) {
        result.error = data.error;
        setStatus("failed");
      }
      return;
    }
    if (
      type === "response.output_item.added" ||
      type === "response.output_item.done"
    ) {
      const item = object(data.item);
      if (item) {
        recognized = true;
        responseItem(item, itemIndex(data, item));
      } else result.unknown.push(value);
      return;
    }
    if (
      type === "response.output_text.delta" ||
      type === "response.text.delta" ||
      type === "response.output_text.done" ||
      type === "response.reasoning_text.delta" ||
      type === "response.reasoning_summary_text.delta" ||
      type === "response.thought.delta" ||
      type === "response.reasoning_text.done" ||
      type === "response.reasoning_summary_text.done"
    ) {
      const done = type.endsWith(".done");
      const text = string(done ? data.text : data.delta);
      if (text === null) {
        result.unknown.push(value);
        return;
      }
      recognized = true;
      const state = choice(itemIndex(data));
      state.id = string(data.item_id) ?? state.id;
      const partIndex = index(data.content_index, index(data.summary_index, 0));
      part(
        state,
        type.includes("reasoning") || type.includes("thought")
          ? "reasoning"
          : "text",
        partIndex,
        text,
        !done,
      );
      return;
    }
    if (
      type === "response.function_call_arguments.delta" ||
      type === "response.function_call_arguments.done"
    ) {
      const done = type.endsWith(".done");
      const args = string(done ? data.arguments : data.delta);
      if (args === null) {
        result.unknown.push(value);
        return;
      }
      recognized = true;
      const number = itemIndex(data);
      const state = choice(number);
      state.id = string(data.item_id) ?? state.id;
      const call = tool(state, number);
      call.id = string(data.call_id) ?? call.id;
      call.name = string(data.name) ?? call.name;
      call.arguments = done ? args : call.arguments + args;
      return;
    }
    if (
      data.object === "response" ||
      Array.isArray(data.output) ||
      typeof data.output_text === "string"
    ) {
      recognized = true;
      response(data);
      return;
    }
    if (data.error != null) {
      recognized = true;
      result.error = data.error;
      setStatus("failed");
      return;
    }
    result.unknown.push(
      name && data.type == null ? { event: name, data: value } : value,
    );
  };

  if (typeof body === "string") {
    const looksLikeSse =
      /^(?:\uFEFF)?(?:data|event|id|retry):|^(?:\uFEFF)?:/m.test(body);
    if (looksLikeSse) {
      result.format = "sse";
      const lines = body.replace(/^\uFEFF/, "").split(/\r\n|\r|\n/);
      let fields: string[] = [];
      let eventName: string | undefined;
      let pending = false;
      const dispatch = () => {
        if (fields.length) {
          const payload = fields.join("\n");
          if (payload.trim() === "[DONE]") {
            terminal = true;
            setStatus("complete");
          } else {
            try {
              event(JSON.parse(payload) as unknown, eventName);
            } catch {
              result.unknown.push(payload);
              malformed = true;
            }
          }
        }
        fields = [];
        eventName = undefined;
        pending = false;
      };
      for (let i = 0; i < lines.length; i++) {
        const line = lines[i];
        if (line === "") {
          // A single trailing newline does not terminate an SSE event.
          if (i !== lines.length - 1) dispatch();
        } else if (!line.startsWith(":")) {
          pending = true;
          const colon = line.indexOf(":");
          const field = colon < 0 ? line : line.slice(0, colon);
          let value = colon < 0 ? "" : line.slice(colon + 1);
          if (value.startsWith(" ")) value = value.slice(1);
          if (field === "data") fields.push(value);
          else if (field === "event") eventName = value;
        }
      }
      if (pending) {
        dispatch();
        malformed = true;
      }
      if (!terminal || malformed) setStatus("incomplete");
    } else {
      try {
        result.format = "json";
        event(JSON.parse(body) as unknown);
      } catch {
        result.unknown.push(body);
      }
    }
  } else if (body != null) {
    result.format = "json";
    event(body);
  }
  if (result.format === "json" && !recognized) result.format = "unknown";
  result.choices = [...choices.values()]
    .sort((a, b) => a.index - b.index)
    .map((state) => ({
      index: state.index,
      id: state.id,
      text: [...state.textParts]
        .sort(([a], [b]) => a - b)
        .map(([, text]) => text)
        .join(""),
      reasoning: [...state.reasoningParts]
        .sort(([a], [b]) => a - b)
        .map(([, text]) => text)
        .join(""),
      toolCalls: state.toolCalls.sort((a, b) => a.index - b.index),
      finishReason: state.finishReason,
    }));
  return result;
}
