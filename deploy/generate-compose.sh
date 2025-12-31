#!/bin/bash
# Generate Docker Compose YAML from .env file for TrueNAS Scale
#
# Usage: ./generate-compose.sh [path/to/.env]
# Then paste the output into TrueNAS: Apps > Discover > Install via YAML

set -e

ENV_FILE="${1:-.env}"

if [ ! -f "$ENV_FILE" ]; then
    echo "Error: .env file not found at $ENV_FILE" >&2
    echo "Usage: $0 [path/to/.env]" >&2
    exit 1
fi

# Source the .env file
set -a
source "$ENV_FILE"
set +a

# Validate required variables
if [ -z "$UNIFI_HOST" ] || [ -z "$UNIFI_USERNAME" ] || [ -z "$UNIFI_PASSWORD" ]; then
    echo "Error: UNIFI_HOST, UNIFI_USERNAME, and UNIFI_PASSWORD are required in .env" >&2
    exit 1
fi

# Warn if RP_ORIGIN not set (needed for passkeys)
if [ -z "$RP_ORIGIN" ]; then
    echo "Warning: RP_ORIGIN not set. Passkeys won't work without it." >&2
    echo "Add to .env: RP_ORIGIN=http://your-truenas-ip:30080" >&2
    echo "" >&2
fi

# Generate Docker Compose YAML
cat <<EOF
services:
  unifi-monitor:
    image: ghcr.io/cynary/unifi-monitoring:latest
    container_name: unifi-monitor
    restart: unless-stopped
    ports:
      - "${LISTEN_PORT:-30080}:8080"
    environment:
      - UNIFI_HOST=${UNIFI_HOST}
      - UNIFI_USERNAME=${UNIFI_USERNAME}
      - UNIFI_PASSWORD=${UNIFI_PASSWORD}
EOF

# Add optional Telegram config if present
if [ -n "$TELEGRAM_BOT_TOKEN" ] && [ -n "$TELEGRAM_CHAT_ID" ]; then
cat <<EOF
      - TELEGRAM_BOT_TOKEN=${TELEGRAM_BOT_TOKEN}
      - TELEGRAM_CHAT_ID=${TELEGRAM_CHAT_ID}
EOF
fi

# Add RP_ORIGIN and RP_ID if set (required for passkeys)
if [ -n "$RP_ORIGIN" ]; then
cat <<EOF
      - RP_ORIGIN=${RP_ORIGIN}
      - RP_ID=${RP_ID:-localhost}
EOF
fi

cat <<EOF
      - DATABASE_PATH=/data/unifi-monitor.db
      - SETUP_TOKEN_PATH=/data/setup-token.txt
      - LOG_DIR=/data/logs
      - LOG_MAX_SIZE_MB=${LOG_MAX_SIZE_MB:-512}
      - DB_MAX_SIZE_MB=${DB_MAX_SIZE_MB:-512}
    volumes:
      - unifi-monitor-data:/data

volumes:
  unifi-monitor-data:
EOF
