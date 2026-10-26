# Gateway Integration

`zero-g` is designed to sit behind an OpenAI-compatible gateway such as
LiteLLM or Bifrost. The gateway owns routing, virtual keys, budgets,
observability, and failover; `zero-g` owns Antigravity OAuth refresh and
protocol translation.

Both `/v1/chat/completions` and `/v1/responses` are exposed so the gateway can
use direct pass-through in either direction. `/v1/models` is also exposed for
model discovery.

## LiteLLM

Configure each model as an `openai/` deployment with `zero-g` as the API base:

```yaml
model_list:
  - model_name: agy-claude
    litellm_params:
      model: openai/claude-sonnet-5-5-medium
      api_base: http://zero-g:8080/agy/v1
  - model_name: zen-beam
    litellm_params:
      model: openai/beam-free
      api_base: http://zero-g:8080/zen/v1
```

LiteLLM calls `zero-g`'s `/agy/v1/chat/completions` for chat traffic and
forwards `/v1/responses` requests to `zero-g`'s `/agy/v1/responses` endpoint. No
Responses-to-chat bridge is required. `zero-g` accepts no bearer auth;
authentication lives on the LiteLLM side.

## Bifrost

Configure `zero-g` as a custom OpenAI-base provider. `base_url` omits `/v1`
because Bifrost appends its OpenAI paths:

```json
{
  "providers": {
    "zero-g": {
      "keys": [
        { "name": "zero-g", "value": "local-only", "models": ["*"] }
      ],
      "network_config": { "base_url": "http://zero-g:8080" },
      "custom_provider_config": {
        "base_provider_type": "openai",
        "allowed_requests": {
          "list_models": true,
          "chat_completion": true,
          "chat_completion_stream": true,
          "responses": true,
          "responses_stream": true
        }
      }
    }
  }
}
```

`zero-g` terminates streams with `data: [DONE]`, so leave
`does_not_send_done_marker` unset. Chat streaming honors
`stream_options.include_usage`, so Bifrost's usage tracking works without
additional configuration.

## Notes

- `zero-g` is keyless and listens on localhost; the provider key above is a
  placeholder (`zero-g` ignores bearer auth). Enforce authentication at the
  gateway (virtual keys, tinyauth, etc.).
- Model names are upstream wire IDs — `/agy/v1/models`, `/codex/v1/models`, and
  `/zen/v1/models` list exactly what each backend serves. There is no alias
  layer; do friendly naming in the gateway.
- Keep `zero-g` reachable only from the gateway: bind it to a private
  interface and let the gateway enforce authentication.
