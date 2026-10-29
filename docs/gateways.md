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
      model: openai/agy/claude-sonnet-5-5-medium
      api_base: http://zero-g:8080/v1
      api_key: os.environ/ZERO_G_VIRTUAL_KEY
  - model_name: zen-beam
    litellm_params:
      model: openai/zen/beam-free
      api_base: http://zero-g:8080/v1
      api_key: os.environ/ZERO_G_VIRTUAL_KEY
```

LiteLLM can call `zero-g`'s `/v1/chat/completions` for chat traffic and
`/v1/responses` for Responses requests. Zen forwards requests to the official
API for all models; availability depends on the upstream. Supply the created
key securely via the gateway environment.

## Bifrost

Configure one custom OpenAI-base provider. Bifrost appends its OpenAI paths, so
the base URL is the zero-g origin. Use the provider-prefixed model IDs from
`/v1/models`.

```json
{
  "providers": {
    "zero-g": {
      "keys": [
        { "name": "zero-g", "value": "<zero-g-virtual-key>", "models": ["*"] }
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

Codex and Antigravity translated Chat Completions streams emit `data: [DONE]`
on success and report an error without a success marker if they end without a
terminal response. Antigravity emits a final usage chunk when requested with
`stream_options.include_usage` and usage is available; Codex does not emit
streaming usage. Go, Zen and custom OpenAI-compatible providers forward
upstream streams. Their termination and usage reporting depend on the upstream.

## Notes

- The provider key must be a valid `zero-g` virtual key; restrict it to the
  providers and models needed by the gateway. Keep the value secret.
- Model IDs are `<provider-id>/<upstream-model-id>`, as returned by the shared
  `/v1/models` endpoint. Do friendly naming in the gateway.
- Keep `zero-g` reachable only from the gateway: bind it to a private
  interface and let the gateway enforce authentication.
