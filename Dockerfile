# Stage 1: Build React frontend
FROM node:20-alpine AS frontend
WORKDIR /app
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci
COPY frontend/ ./
RUN npm run build

# Stage 2: Build Rust backend
FROM rust:1.85-alpine AS backend
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig
WORKDIR /app
# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
# Build with static linking for Alpine
ENV OPENSSL_STATIC=1
RUN cargo build --release

# Stage 3: Final minimal image
FROM alpine:3.21
RUN apk add --no-cache ca-certificates
WORKDIR /app
COPY --from=backend /app/target/release/unifi-monitor .
COPY --from=frontend /app/dist ./static/
VOLUME /data
ENV DATABASE_PATH=/data/unifi-monitor.db
ENV SETUP_TOKEN_PATH=/data/setup-token.txt
ENV LOG_DIR=/data/logs
ENV LOG_MAX_SIZE_MB=512
ENV STATIC_DIR=/app/static
EXPOSE 8080
CMD ["./unifi-monitor"]
