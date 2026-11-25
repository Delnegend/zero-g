# Error Handling & Wire Invariants

| Behavior                | Provider Scope | Upstream Handling                                                                                                                                            |
| ----------------------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **Usage Chunks**        | Codex, Cursor  | Emits final usage chunk when `stream_options.include_usage: true` is requested.                                                                              |
| **Stream Termination**  | Codex          | Emits `data: [DONE]` on clean close. Interrupted streams yield an error event instead of a silent finish.                                                    |
| **Unsupported Limits**  | Codex          | Accepts but does not forward `max_tokens` and `max_completion_tokens`; an effective output cap is unknown.                                                   |
| **Strict Wire Options** | Codex, Cursor  | Reject unsupported options (e.g. unknown structured formats, stop sequences, unsupported choice counts) with clear errors instead of dropping them silently. |

See [Gateways](gateways.md) for the endpoints these rules apply to.
