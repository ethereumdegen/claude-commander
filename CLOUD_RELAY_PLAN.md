# Cloud WebSocket Relay Service

## Purpose
A lightweight cloud service that acts as a WebSocket message router, enabling remote clients to connect to Claude Commander instances without requiring port forwarding or direct network access. The relay is a dumb pipe — it authenticates connections and forwards messages between a "host" (the Commander TUI) and one or more "clients" (remote controllers).

## Architecture

```
┌──────────────────┐         ┌─────────────────┐         ┌──────────────┐
│ Claude Commander │ ──ws──▶ │  Cloud Relay    │ ◀──ws── │ Remote Client│
│ (TUI host)       │         │  (router)       │         │ (browser/cli)│
└──────────────────┘         └─────────────────┘         └──────────────┘
        │                           │                           │
        │  register as host         │                           │
        │  room=abc123              │                           │
        │  key=secret               │                           │
        │◀─── ack ─────────────────│                           │
        │                           │     join room=abc123      │
        │                           │     key=secret            │
        │                           │───── ack ────────────────▶│
        │                           │                           │
        │◀──── relay(msg) ─────────│◀──── {"action":"list"}───│
        │───── {"type":"sessions"} │───── relay(msg) ─────────▶│
```

## Tech Stack
- **Runtime:** Node.js or Rust (axum/warp) — either works, Node is faster to build
- **WebSocket library:** `ws` (Node) or `axum` + `tokio-tungstenite` (Rust)
- **Deployment:** Single container on Fly.io / Railway / any VPS
- **No database needed** — all state is in-memory (rooms are ephemeral)

## Core Concepts

### Rooms
- A **room** is created when a Commander host connects
- Room ID = first 16 chars of the secret key's SHA-256 hash (deterministic, non-reversible)
- Room is destroyed when the host disconnects
- Multiple clients can join the same room

### Authentication
- Both host and client must provide the same secret key
- Relay verifies: `sha256(provided_key)[0..16] == room_id`
- Key is never stored — only the hash is compared
- Optional: rate limiting on failed auth attempts

## API

### Host Connection
```
GET wss://relay.example.com/host
Headers: X-Room-Id: <room_id>, X-Key: <secret_key>
```
- Creates room if it doesn't exist
- Only one host per room (rejects if host already connected)
- On disconnect: room is destroyed, all clients disconnected

### Client Connection
```
GET wss://relay.example.com/join
Headers: X-Room-Id: <room_id>, X-Key: <secret_key>
```
- Joins existing room
- Rejected if room doesn't exist or key doesn't match
- Multiple clients allowed per room

### Message Flow
- All messages from a client are forwarded to the host verbatim
- All messages from the host are broadcast to all clients in the room
- Relay adds a thin envelope for client identification:

**Client → Host (wrapped by relay):**
```json
{"from": "client_abc", "payload": {"action": "list_sessions"}}
```

**Host → Client(s) (wrapped by relay):**
```json
{"to": "client_abc", "payload": {"type": "sessions", "data": [...]}}
// or broadcast (no "to" field)
{"payload": {"type": "event", "session_id": 1, ...}}
```

## Data Structures

```typescript
// Node.js version
interface Room {
  id: string;                    // room ID (hash-derived)
  host: WebSocket;               // the Commander connection
  clients: Map<string, WebSocket>; // client_id → socket
  createdAt: Date;
}

// In-memory store
const rooms = new Map<string, Room>();
```

## Implementation Plan

### Phase 1: Minimal Relay (MVP)
1. WebSocket server with `/host` and `/join` endpoints
2. Room creation/destruction on host connect/disconnect
3. Key-based authentication
4. Bidirectional message forwarding
5. Health check endpoint (`GET /health`)

### Phase 2: Robustness
1. Heartbeat/ping-pong to detect dead connections (30s interval)
2. Rate limiting (max 100 messages/sec per connection)
3. Max message size (1MB)
4. Max clients per room (10)
5. Room TTL (auto-destroy after 24h)
6. Graceful shutdown

### Phase 3: Observability
1. Metrics endpoint (`GET /metrics`) — active rooms, connections, messages/sec
2. Structured logging (JSON)
3. Optional: admin endpoint to list/kill rooms

## Security Considerations
- **No data persistence** — messages are never stored, only forwarded
- **Key validation** — SHA-256 hash comparison, no plaintext storage
- **TLS required** — WSS only in production
- **Rate limiting** — prevent abuse
- **No authentication tokens** — the secret key IS the auth; rotate by restarting Commander
- **Room isolation** — clients can only see rooms they have the key for

## Deployment

### Docker
```dockerfile
FROM node:20-slim
WORKDIR /app
COPY package*.json ./
RUN npm ci --production
COPY . .
EXPOSE 8080
CMD ["node", "server.js"]
```

### Fly.io (example)
```toml
[http_service]
  internal_port = 8080
  force_https = true

[[services.ports]]
  handlers = ["tls", "http"]
  port = 443
```

### Environment Variables
- `PORT` — listen port (default 8080)
- `MAX_ROOMS` — max concurrent rooms (default 1000)
- `MAX_CLIENTS_PER_ROOM` — (default 10)
- `ROOM_TTL_HOURS` — auto-cleanup (default 24)

## Estimated Scope
- **MVP:** ~200 lines of code (Node.js) or ~400 lines (Rust)
- **Full implementation:** ~500 lines (Node.js) or ~800 lines (Rust)
- **Deploy time:** <1 hour with Fly.io/Railway
