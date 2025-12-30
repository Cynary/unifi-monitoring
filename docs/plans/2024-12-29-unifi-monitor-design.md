# UniFi Monitor - Design Document

## Overview

A self-hosted event monitoring system for UniFi devices. Collects events from UniFi Protect, Network, and System websocket APIs, stores them in SQLite, classifies them into categories, and sends Telegram notifications for events marked as "notify".

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Docker Container                         │
│  ┌─────────────────────────────────────────────────────┐    │
│  │              unifi-monitor (Rust binary)             │    │
│  │                                                      │    │
│  │  ┌──────────────┐  ┌──────────────┐  ┌───────────┐  │    │
│  │  │ UniFi Client │  │  Web Server  │  │ Telegram  │  │    │
│  │  │  (events)    │  │   (Axum)     │  │   Bot     │  │    │
│  │  └──────┬───────┘  └──────┬───────┘  └─────┬─────┘  │    │
│  │         │                 │                │        │    │
│  │         └────────┬────────┴────────┬───────┘        │    │
│  │                  │                 │                │    │
│  │             ┌────▼────┐      ┌─────▼─────┐          │    │
│  │             │ SQLite  │      │  /static  │          │    │
│  │             │  (DB)   │      │  (React)  │          │    │
│  │             └─────────┘      └───────────┘          │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
         │                              ▲
         ▼                              │
┌─────────────────┐            ┌────────┴────────┐
│   UDM SE        │            │   Your Browser  │
│ (WebSockets)    │            │   + Passkey     │
└─────────────────┘            └─────────────────┘
```

**Single Rust binary** with three main components:

1. **UniFi Client** - Connects to all three WebSockets + handles reconnection
2. **Web Server** - Axum serving React UI + API endpoints
3. **Telegram Bot** - Sends notifications for "notify" events

## Technology Stack

| Component | Technology |
|-----------|------------|
| Language | Rust |
| Web Framework | Axum |
| Database | SQLite + FTS5 |
| Frontend | React |
| Auth | Passkeys (WebAuthn via `webauthn-rs`) |
| Notifications | Telegram Bot API |
| Container | Alpine-based (~20-30MB) |
| CI/CD | GitHub Actions → ghcr.io |

## UniFi Event Collection

### WebSocket Endpoints

| Endpoint | Format | Events |
|----------|--------|--------|
| `/proxy/network/wss/s/default/events` | JSON | `alarm`, `notification`, `device:sync`, `sta:sync`, `evt` |
| `/api/ws/system` | JSON | Cross-app OS-level events |
| `/proxy/protect/ws/updates` | Binary | NVR status, storage health, camera events |

### Authentication Flow

1. GET `/` → fetch CSRF token from headers
2. POST `/api/auth/login` with credentials + CSRF token
3. Store session cookie + updated CSRF token

### Connection & Reconnection

```
On startup:
1. Check DB for last stored updateId
2a. If exists → Connect WebSocket with stored updateId
    - If succeeds: stream from where we left off
    - If fails: go to 2b
2b. If not exists / failed →
    - GET bootstrap (includes current state + recent events)
    - Connect WebSocket with bootstrap's lastUpdateId
    - Dedup bootstrap events against DB
```

### Protect Binary Protocol

```
[Header: 8 bytes] [Action Frame: JSON] [Header: 8 bytes] [Data Frame: JSON]

Header format:
- Byte 0: Packet type (1=action, 2=payload)
- Byte 1: Format (1=JSON, 2=UTF8, 3=Buffer)
- Byte 2: Compressed (0/1, zlib)
- Byte 3: Reserved
- Bytes 4-7: Payload size (big endian)
```

### Unified Event API

```rust
pub struct UnifiClient { /* ... */ }

impl UnifiClient {
    pub async fn connect(config: &UnifiConfig) -> Result<Self>;
    pub fn events(&self) -> impl Stream<Item = UnifiEvent>;
}

pub struct UnifiEvent {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub source: EventSource,  // Protect | Network | System
    pub event_type: String,
    pub severity: Option<Severity>,
    pub raw: serde_json::Value,
}
```

## Event Classification

### Three States

1. **Ignored** - Reviewed, won't notify (still stored)
2. **Unclassified** - Not yet reviewed, won't notify (needs attention)
3. **Notify** - Reviewed, sends Telegram alert

### Database Schema

```sql
-- Event type classifications
CREATE TABLE event_type_rules (
    event_type TEXT PRIMARY KEY,
    classification TEXT NOT NULL,  -- "ignored" | "notify"
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- All events (stored regardless of classification)
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    event_type TEXT NOT NULL,
    severity TEXT,
    payload TEXT NOT NULL,
    summary TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    notified INTEGER DEFAULT 0,
    notify_attempts INTEGER DEFAULT 0,
    created_at INTEGER NOT NULL
);

-- Full-text search
CREATE VIRTUAL TABLE events_fts USING fts5(
    event_type,
    summary,
    content='events',
    content_rowid='rowid'
);

-- Sync state
CREATE TABLE sync_state (
    source TEXT PRIMARY KEY,
    last_update_id TEXT,
    updated_at INTEGER NOT NULL
);
```

### Classification Flow

```
Event arrives → Lookup event_type_rules
  → Rule exists: apply "ignored" or "notify"
  → No rule: default to "unclassified"

If "notify" → store + queue for Telegram
Otherwise → store only
```

### Retention

- Max DB size: 512MB (configurable via `DB_MAX_SIZE_MB`)
- Cleanup: delete oldest events when limit exceeded
- Run on startup + periodically

## Telegram Notifications

### Delivery Guarantees

At-least-once delivery using a queue pattern:

```
┌─────────────┐    ┌──────────────┐    ┌───────────────┐
│ UniFi Event │───▶│ Classify &   │───▶│ mpsc channel  │
│   Stream    │    │ Store in DB  │    │ (in-memory)   │
└─────────────┘    │ notified=F   │    └───────┬───────┘
                   └──────────────┘            │
                                               ▼
                                    ┌──────────────────┐
                                    │ Telegram Sender  │
                                    │ • Read from chan │
                                    │ • Send to TG     │
                                    │ • Retry w/backoff│
                                    │ • Update DB      │
                                    └──────────────────┘

Startup:
1. Query: SELECT * FROM events WHERE notified = false AND classification = 'notify'
2. Push all into mpsc channel
3. Start sender task
```

### Retry Logic

- Exponential backoff: 1s, 2s, 4s, 8s, ...
- Max attempts: 10 (configurable)
- After max failures: mark as `notify_failed`

## Web UI

### Structure

```
┌─────────────────────────────────────────────────────────────────┐
│  UniFi Event Monitor                          [Search...]       │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ⚠️  UNCLASSIFIED EVENT TYPES (prominent)                       │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ nvr.storage_warning                                      │   │
│  │ Latest: "Storage pool degraded..." (2 min ago)           │   │
│  │ [Ignore] [Notify]                                        │   │
│  └─────────────────────────────────────────────────────────┘   │
│                                                                 │
│  🔔 Notify (collapsed)     🔇 Ignored (collapsed)              │
│                                                                 │
├─────────────────────────────────────────────────────────────────┤
│  📋 All Events                                                  │
│  Filters: ☑️ Notify  ☑️ Ignored  ☑️ Unclassified                │
│  Classes: [dropdown multi-select]                               │
│  Search:  [full-text / regex toggle]                           │
│                                                                 │
│  [Virtual scrolling event list - lazy loaded]                   │
└─────────────────────────────────────────────────────────────────┘
```

### Features

- **Search**: Full-text (FTS5) + fuzzy matching + regex support
- **Live updates**: WebSocket push from backend to UI
- **Virtual scrolling**: `@tanstack/react-virtual` for performance
- **Filters**: By classification, event type, search query

## Authentication

### Passkey-Only Auth

No usernames/passwords. Single implicit owner with multiple passkeys.

### Schema

```sql
CREATE TABLE passkeys (
    id TEXT PRIMARY KEY,
    public_key BLOB NOT NULL,
    counter INTEGER NOT NULL,
    name TEXT,
    created_at INTEGER NOT NULL
);

CREATE TABLE setup_token (
    token TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);

CREATE TABLE invite_tokens (
    token TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
```

### Flows

**First Passkey (no passkeys exist):**
1. Server writes setup token to `/data/setup-token.txt`
2. User enters token + registers passkey
3. Setup token deleted

**Add Passkey (logged in on Device A):**
1. Generate invite phrase (e.g., "correct-horse-battery-staple")
2. Expires in 5 minutes (configurable)
3. Use phrase on Device B to register new passkey

**Delete All Passkeys:**
- Regenerates setup token
- Invalidates all sessions

## Deployment

### Dockerfile

```dockerfile
# Stage 1: Build React frontend
FROM node:20-alpine AS frontend
WORKDIR /app
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# Stage 2: Build Rust backend
FROM rust:1.75-alpine AS backend
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release --target x86_64-unknown-linux-musl

# Stage 3: Final image
FROM alpine:3.19
RUN apk add --no-cache ca-certificates
WORKDIR /app
COPY --from=backend /app/target/x86_64-unknown-linux-musl/release/unifi-monitor .
COPY --from=frontend /app/dist ./static/
VOLUME /data
ENV DATABASE_PATH=/data/unifi-monitor.db
ENV SETUP_TOKEN_PATH=/data/setup-token.txt
EXPOSE 8080
CMD ["./unifi-monitor"]
```

### GitHub Actions

- **CI** (on push/PR): fmt, clippy, test, frontend lint/typecheck/build
- **Release** (on tag): Build & push to `ghcr.io/cynary/unifi-monitor`

### TrueNAS Scale

Deploy as custom app using Kubernetes YAML with:
- PersistentVolumeClaim for `/data`
- Secrets for credentials
- LoadBalancer or Ingress for external access

## Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `UNIFI_HOST` | Yes | - | UDM IP/hostname |
| `UNIFI_USERNAME` | Yes | - | Local admin user |
| `UNIFI_PASSWORD` | Yes | - | Password |
| `TELEGRAM_BOT_TOKEN` | Yes | - | Bot token |
| `TELEGRAM_CHAT_ID` | Yes | - | Your Telegram ID |
| `DATABASE_PATH` | No | `/data/unifi-monitor.db` | SQLite path |
| `SETUP_TOKEN_PATH` | No | `/data/setup-token.txt` | Setup token file |
| `LISTEN_ADDR` | No | `0.0.0.0:8080` | HTTP bind address |
| `DB_MAX_SIZE_MB` | No | `512` | Max DB size |
| `INVITE_TOKEN_EXPIRY_SECS` | No | `300` | Passkey invite expiry |
| `SESSION_EXPIRY_DAYS` | No | `30` | Session duration |
| `TELEGRAM_MAX_RETRIES` | No | `10` | Max send attempts |
