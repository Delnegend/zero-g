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
        Dispatch --> Codex[Codex]
        Dispatch --> Cursor[Cursor]
        Dispatch --> ChatJimmy[ChatJimmy]
        Dispatch --> CommandCodeGo[Command Code Go]
    end

    subgraph Passthrough ["Passthrough / Forwarded"]
        Dispatch --> Claude[Claude Code /v1/messages]
        Dispatch --> Zen[OpenCode Zen]
        Dispatch --> Go[OpenCode Go]
        Dispatch --> OpenRouter[OpenRouter + Decisions API]
        Dispatch --> CommandCode[Command Code]
        Dispatch --> Codev[CoDev]
        Dispatch --> Custom[Custom OpenAI-Compatible]
    end

    Codex -->|ChatGPT Backend| OpenAI[ChatGPT Backend]
    Claude --> Anthropic[Anthropic API]
    
    V1 -.->|Log Request / Response| Logs[Observability Engine]
    Logs -.-> DB
```

---

## Core Components

Where each part lives is in the code map in [Development](development.md#architecture--code-map).

| Component                | Responsibility                                                                                                                                            |
| ------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **HTTP Layer**           | Axum web server exposing `/v1` inference and dashboard endpoints.                                                                                         |
| **Dashboard Auth**       | Maps configuration to `axum-auth-gate` (Basic and OIDC + PKCE).                                                                                           |
| **Protocol Types**       | Strongly typed Serde models for Chat Completions and Responses API.                                                                                       |
| **Auth Managers**        | Token lifecycle, credential locking, and provider-specific refreshes. OAuth providers keep their stored credential fields private in their own `auth.rs`. |
| **Provider System**      | Provider traits for routing and model discovery.                                                                                                          |
| **Credential Pools**     | Encrypted credential storage and failover selection; see [Credential Pool & Failover Logic](configuration.md#credential-pool--failover-logic).            |
| **Catalog & Datasheets** | Model metadata, pricing synchronization, and background discovery polling.                                                                                |
| **Storage & Migrations** | Toasty ORM over SQLite/Turso with retry-aware transactions and startup migrations.                                                                        |

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

    Client->>V1: POST /v1/chat/completions (model: "codex/gpt-5.5")
    V1->>Auth: Verify Virtual Key & Model Permissions
    Auth-->>V1: Authorized
    V1->>Pool: Select credential when required
    Pool-->>V1: Credential (OAuth token or API key)
    
    alt Needs Token Refresh (expires in < 60s)
        V1->>Upstream: Refresh OAuth token
        Upstream-->>V1: New Access Token
    end

    V1->>Engine: Translate OpenAI payload to Upstream format
    Engine->>Upstream: Send translated request
    
    alt Upstream Error (401, 403, 429 before streaming)
        Upstream-->>Engine: Failover status code
        Engine->>Pool: Request next eligible credential
        Pool-->>Engine: Alternative Credential
        Engine->>Upstream: Retry request
    end

    Upstream-->>Engine: Stream chunks (SSE)
    Engine-->>V1: Translate to OpenAI stream chunks
    V1-->>Client: Stream SSE chunks
    V1-)Auth: Async logging
```

Per-provider wire guarantees (streaming, usage chunks, strict options) are in [Wire invariants](wire-invariants.md); size and retention limits are in [Configuration](configuration.md#request--quota-limits).
