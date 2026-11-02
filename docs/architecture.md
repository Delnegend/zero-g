# Architecture

Zero-G provides a unified OpenAI-compatible interface (`/v1`) that translates and dispatches requests across multiple upstream LLM providers, with credential pooling, local SQLite/Turso persistence, and an integrated management dashboard.

```mermaid
flowchart TD
    Client[Client / Gateway / Agent] -->|Bearer Virtual Key + provider/model| V1["Unified /v1 API"]
    Admin[Dashboard User] -->|Basic / OIDC Auth| Dash[Dashboard API]
    
    subgraph Storage ["Persistence (Turso/SQLite)"]
        DB[(Local DB)]
    end

    V1 -->|Validate Key| DB
    Dash -->|Manage Credentials & Settings| DB

    V1 --> Dispatch[Provider Dispatch]
    Dispatch -->|Load Active Credentials| DB

    subgraph Translation ["Translated Providers"]
        Dispatch --> AGY[Antigravity]
        Dispatch --> Codex[Codex]
        Dispatch --> Claude[Claude Code]
        Dispatch --> Cursor[Cursor]
        Dispatch --> Grok[Grok / Grok Build]
    end

    subgraph Passthrough ["Passthrough / Forwarded"]
        Dispatch --> Zen[OpenCode Zen]
        Dispatch --> Go[OpenCode Go]
        Dispatch --> OpenRouter[OpenRouter + Decisions API]
        Dispatch --> Custom[Custom OpenAI-Compatible]
    end

    AGY -->|Upstream SSE / gRPC| Google[Google Cloud Code Assist]
    Codex -->|ChatGPT Backend| OpenAI[OpenAI API]
    Claude -->|Interleaved Thinking| Anthropic[Anthropic API]
    
    V1 -.->|Log Request / Response| Logs[Observability Engine]
    Logs -.-> DB
```

---

## Core Components

| Component | Path / Location | Responsibility |
|---|---|---|
| **HTTP Layer** | `backend/server/` | Axum web server exposing `/v1` inference and dashboard endpoints. |
| **Dashboard Auth** | `backend/server/dashboard_auth_config.rs` | Maps configuration to `axum-auth-gate` (Basic and OIDC + PKCE). |
| **Protocol Types** | `backend/openai.rs`, `backend/responses.rs` | Strongly typed Serde models for Chat Completions and Responses API. |
| **Auth Managers** | `backend/provider/*/auth.rs` | Token lifecycle, credential locking, and provider-specific refreshes. |
| **Provider System** | `backend/provider/common/`, `backend/provider/*/` | `Provider` trait for routing, discovery, streaming, and error normalization. |
| **Credential Pools** | `backend/provider/credentials.rs` | Encrypted credential storage, weighted load-balancing, and automatic failover on 401/403/429. |
| **Catalog & Datasheets** | `backend/provider/catalog.rs`, `backend/server/model_setting/` | Model metadata, pricing synchronization, and background discovery polling. |
| **Storage & Migrations** | `backend/database.rs`, `backend/storage.rs` | Toasty ORM over SQLite/Turso with retry-aware transactions and startup migrations. |

---

## Request & Data Flow

```mermaid
sequenceDiagram
    autonumber
    actor Client
    participant V1 as /v1 Router
    participant Auth as Auth & DB
    participant Pool as Credential Pool
    participant Engine as Provider Engine
    participant Upstream as Upstream Service

    Client->>V1: POST /v1/chat/completions (model: "agy/claude-sonnet-4")
    V1->>Auth: Verify Virtual Key & Model Permissions
    Auth-->>V1: Authorized
    V1->>Pool: Borrow weighted connected credential
    Pool-->>V1: Credential (with OAuth access token)
    
    alt Needs Token Refresh (expires in < 60s)
        V1->>Upstream: Refresh OAuth token
        Upstream-->>V1: New Access Token
    end

    V1->>Engine: Translate OpenAI payload to Upstream format
    Engine->>Upstream: Send translated request
    
    alt Upstream Error (401, 403, 429 before streaming)
        Upstream-->>Engine: Failover status code
        Engine->>Pool: Mark credential temporary failure, request next
        Pool-->>Engine: Alternative Credential
        Engine->>Upstream: Retry request
    end

    Upstream-->>Engine: Stream chunks (SSE)
    Engine-->>V1: Translate to OpenAI stream chunks
    V1-->>Client: Stream SSE chunks
    V1-)Auth: Async log request & response metadata (capped at 256 KiB)
```

---

## Invariants & Protocol Guarantees

| Invariant | Scope | Rule |
|---|---|---|
| **OAuth Token Refresh** | Antigravity, Codex | Checked before request; if expiration is within 60s, token refresh triggers automatically. |
| **Endpoint Failover** | Antigravity | Tries `daily-cloudcode-pa.googleapis.com` first; falls back to sandbox endpoint on network failure or 5xx. |
| **Thought Signatures** | Antigravity | Preserves reasoning signatures via `provider_parts` and `thought_signature`. Missing first-tool signature triggers `skip_thought_signature_validator`. |
| **Contiguous Tool Turns** | Cloud Code Assist | Successive tool output turns are merged into a single contiguous `user` role turn. |
| **Interrupted Streams** | Antigravity, Codex | Broken streams without terminal response raise a clean downstream error instead of a truncated success marker. |
| **Claude Wire Handling** | Claude (via AGY) | Includes `anthropic-beta: interleaved-thinking-2025-05-14`; tool mode defaults to `VALIDATED`; output tokens capped to catalog limit. |
| **Payload Size Limit** | All `/v1` Endpoints | Requests capped at 20,000,000 bytes. Responses logged up to 256 KiB. |
