# Gateway Integration

`zero-g` is designed to sit behind an OpenAI-compatible gateway such as
LiteLLM or Bifrost. The gateway can own routing, budgets, observability, and
failover; `zero-g` owns provider authentication and protocol translation. Create
a `zero-g` virtual key in its dashboard for the gateway's upstream requests.

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
      api_base: http://zero-g:8080/agy/agy/v1
      api_key: os.environ/ZERO_G_VIRTUAL_KEY
  - model_name: zen-beam
    litellm_params:
      model: openai/beam-free
      api_base: http://zero-g:8080/zen/zen/v1
      api_key: os.environ/ZERO_G_VIRTUAL_KEY
```

LiteLLM calls `zero-g`'s `/agy/agy/v1/chat/completions` for chat traffic and
forwards `/v1/responses` requests to `zero-g`'s `/agy/agy/v1/responses` endpoint. No
Responses-to-chat bridge is required. Supply the created key securely via the
gateway environment.

## Bifrost

Configure each provider as a custom OpenAI-base provider. Set the appropriate
provider prefix in `base_url` because Bifrost appends its OpenAI paths:

```json
{
  "providers": {
    "zero-g": {
      "keys": [
        { "name": "zero-g", "value": "<zero-g-virtual-key>", "models": ["*"] }
      ],
      "network_config": { "base_url": "http://zero-g:8080/agy/agy" },
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

- The provider key must be a valid `zero-g` virtual key; restrict it to the
  providers and models needed by the gateway. Keep the value secret.
- Model names are upstream wire IDs. Each named provider exposes `/{kind}/{id}/v1/models`
  for model discovery. There is no alias layer; do friendly naming in the gateway.
- Keep `zero-g` reachable only from the gateway: bind it to a private
  interface and let the gateway enforce authentication.
