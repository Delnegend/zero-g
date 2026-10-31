# Gateway Integration

`zero-g` is designed to sit behind an OpenAI-compatible gateway such as
LiteLLM or Bifrost. The gateway can own routing, budgets, observability, and
failover; `zero-g` owns provider authentication and protocol translation. Create
a `zero-g` virtual key in its dashboard for the gateway's upstream requests.

Both `/v1/chat/completions` and `/v1/responses` are exposed so the gateway can
use direct pass-through in either direction. `/v1/models` is also exposed for
model discovery.

OpenRouter Decisions models use a different request format and cannot be called
through Chat Completions or Responses. Send them directly to `POST /v1/decisions`
with a Zero-G virtual key and a provider-qualified model ID:

```bash
curl http://127.0.0.1:8080/v1/decisions \
  -H "Authorization: Bearer $ZERO_G_VIRTUAL_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model":"openrouter/cloudflare/clef","state":"A red square","questions":{"is_red":{"type":"noul","instructions":"Is it red?"}}}'
```

Zero-G forwards the Decisions payload to OpenRouter's `/api/alpha/decisions`.
Other provider kinds do not support this endpoint. Model and provider restrictions
on the virtual key still apply. Multimodal parts and other request fields pass
through unchanged, within Zero-G's 20,000,000-byte request limit. Consult the
[OpenRouter Decisions API](https://openrouter.ai/docs/guides/community/multimodal-decisions)
for supported input formats and model-specific limits.

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

Codex, Cursor and Antigravity translated Chat Completions streams emit a final
usage chunk when requested with `stream_options.include_usage` and usage is
available. Codex and Antigravity emit `data: [DONE]` on success and report an
error without a success marker if they end without a terminal response. Go, Zen
and custom OpenAI-compatible providers forward upstream streams; their
termination and usage reporting depend on the upstream.

Codex rejects `max_tokens` and `max_completion_tokens` in Chat Completions and
`max_output_tokens` in Responses with a 400 error, rather than silently ignoring
the requested limit. Other upstream Codex errors retain their HTTP status after
credential failover.

Translated providers reject request options they cannot honor instead of silently
dropping them. This includes structured output formats, stop sequences, unsupported
choice counts and Responses conversation references. Send the full conversation
when a provider does not support a previous-response reference.

## Notes

- The provider key must be a valid `zero-g` virtual key; restrict it to the
  providers and models needed by the gateway. Keep the value secret.
- Model IDs are `<provider-id>/<upstream-model-id>`, as returned by the shared
  `/v1/models` endpoint. Do friendly naming in the gateway.
- Keep `zero-g` reachable only from the gateway: bind it to a private
  interface and let the gateway enforce authentication.
