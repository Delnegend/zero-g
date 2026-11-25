# Model capability metadata

`GET /v1/models` keeps the OpenAI-compatible `data` array and may add per-model fields.
**An omitted field means unknown**: not `false`, not empty, and never guessed from the model name.

| Field                                                                  | Meaning                                                          |
| ---------------------------------------------------------------------- | ---------------------------------------------------------------- |
| `context_window`                                                       | Effective combined route context limit, in tokens                |
| `max_output_tokens`                                                    | Output cap the route actually enforces                           |
| `input_modalities`, `output_modalities`                                | `text`, `image`, `audio`, `video`, `pdf`, for this gateway route |
| `supports_tools`                                                       | `false` means no end-to-end OpenAI-compatible tool support       |
| `reasoning`, `supported_reasoning_efforts`, `default_reasoning_effort` | Efforts the route accepts; the default only when known           |

| Provider                            | What discovery reports                                                                                                                                                           |
| ----------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Codex                               | No output limit (no verified cap; client output limits are not forwarded) and tool support unknown. Effort strings, even unknown ones, go to upstream; none is set when omitted. |
| Antigravity                         | Not in the OpenAI-format list, because Zero-G does not route it. The catalog page lists the models `agy models` reports, with an id and a display name only.                     |
| Claude                              | Not in the OpenAI-format list. Anthropic clients get Anthropic's own model fields unchanged.                                                                                     |
| Zen, Go, OpenRouter                 | Only route-relevant metadata from their model lists                                                                                                                              |
| ChatJimmy                           | No tools; requests with tool definitions, calls or results are rejected, not stripped                                                                                            |
| Custom OpenAI and other passthrough | Upstream fields kept as is; missing ones are not filled in                                                                                                                       |

See [Gateways](gateways.md) for the endpoints that return this list.
