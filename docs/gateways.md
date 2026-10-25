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
  - model_name: claude-sonnet
    litellm_params:
      model: openai/claude-sonnet
      api_base: http://zero-g:8080/v1
      api_key: os.environ/ZERO_G_API_KEY
  - model_name: gemini-flash
    litellm_params:
      model: openai/gemini-flash
      api_base: http://zero-g:8080/v1
      api_key: os.environ/ZERO_G_API_KEY
```

LiteLLM calls `zero-g`'s `/v1/chat/completions` for chat traffic and forwards
`/v1/responses` requests to `zero-g`'s `/v1/responses` endpoint. No
Responses-to-chat bridge is required.

## Bifrost

Configure `zero-g` as a custom OpenAI-base provider. `base_url` omits `/v1`
because Bifrost appends its OpenAI paths:

```json
{
  "providers": {
    "zero-g": {
      "keys": [
        { "name": "zero-g", "value": "env.ZERO_G_API_KEY", "models": ["*"] }
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

- Set `ZERO_G_API_KEY` in `zero-g` and reuse that value as the gateway's
  provider key.
- Model names may be zero-g aliases (`claude-sonnet`, `gemini-flash`, ...) or
  upstream wire IDs. `/v1/models` returns both.
- If `zero-g` is only reachable from the gateway, bind it to a private
  interface and let the gateway enforce authentication.
