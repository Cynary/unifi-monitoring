# UniFi Monitor

A self-hosted event monitoring system for UniFi devices. Collects events from UniFi Protect, Network, and System APIs, classifies them, and sends Telegram notifications for events you care about.

## Features

- **Unified Event Collection**: Connects to all three UniFi websockets (Protect, Network, System) for comprehensive monitoring
- **Smart Classification**: Three-state system (Ignored / Unclassified / Notify) with persistent rules
- **Telegram Notifications**: At-least-once delivery with retry queue
- **Web UI**: Browse events, classify event types, search with fuzzy matching and regex
- **Passkey Authentication**: Passwordless, phishing-resistant auth with multi-device support
- **Self-Hosted**: Single Docker container, runs on TrueNAS Scale or any Docker host

## Quick Start

### Prerequisites

- UniFi Dream Machine (UDM/UDM Pro/UDM SE) or similar UniFi OS device
- Docker host (TrueNAS Scale, Synology, etc.)
- Telegram account (for notifications)

### 1. Create a Telegram Bot

1. Message [@BotFather](https://t.me/botfather) on Telegram
2. Send `/newbot` and follow prompts
3. Save the bot token
4. Message your new bot, then get your chat ID from [@userinfobot](https://t.me/userinfobot)

### 2. Create UniFi Local User

1. Log into your UniFi console
2. Go to Settings > Admins & Users
3. Create a local admin account (SSO accounts are not supported)

### 3. Deploy

```bash
docker run -d \
  --name unifi-monitor \
  -p 8080:8080 \
  -v /path/to/data:/data \
  -e UNIFI_HOST=192.168.1.1 \
  -e UNIFI_USERNAME=api-user \
  -e UNIFI_PASSWORD=your-password \
  -e TELEGRAM_BOT_TOKEN=123456:ABC... \
  -e TELEGRAM_CHAT_ID=your-id \
  ghcr.io/cynary/unifi-monitor:latest
```

### 4. Initial Setup

1. Check logs for setup token: `docker logs unifi-monitor`
2. Open http://your-host:8080
3. Enter setup token and register your passkey

## Configuration

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `UNIFI_HOST` | Yes | - | UniFi console IP/hostname |
| `UNIFI_USERNAME` | Yes | - | Local admin username |
| `UNIFI_PASSWORD` | Yes | - | Local admin password |
| `TELEGRAM_BOT_TOKEN` | Yes | - | Bot token from @BotFather |
| `TELEGRAM_CHAT_ID` | Yes | - | Your Telegram user ID |
| `DATABASE_PATH` | No | `/data/unifi-monitor.db` | SQLite database path |
| `SETUP_TOKEN_PATH` | No | `/data/setup-token.txt` | Initial setup token file |
| `LISTEN_ADDR` | No | `0.0.0.0:8080` | HTTP listen address |
| `DB_MAX_SIZE_MB` | No | `512` | Max database size before cleanup |
| `INVITE_TOKEN_EXPIRY_SECS` | No | `300` | Passkey invite token expiry |
| `SESSION_EXPIRY_DAYS` | No | `30` | Session duration |
| `TELEGRAM_MAX_RETRIES` | No | `10` | Max notification retry attempts |

## Development

### Prerequisites

- Rust 1.75+
- Node.js 20+
- Docker (for building images)

### Local Development

```bash
# Backend
cargo run

# Frontend (separate terminal)
cd frontend
npm install
npm run dev
```

### Building Docker Image

```bash
docker build -t unifi-monitor .
```

## License

MIT License - see [LICENSE](LICENSE) for details.
