#!/bin/bash
# Generate TrueNAS Kubernetes YAML from .env file
#
# Usage: ./generate-yaml.sh [path/to/.env] > my-deployment.yaml

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

# Generate YAML
cat <<EOF
# Generated from $ENV_FILE
# Apply with: kubectl apply -f <this-file>.yaml

---
apiVersion: v1
kind: Namespace
metadata:
  name: unifi-monitor

---
apiVersion: v1
kind: Secret
metadata:
  name: unifi-monitor-secrets
  namespace: unifi-monitor
type: Opaque
stringData:
  UNIFI_HOST: "$UNIFI_HOST"
  UNIFI_USERNAME: "$UNIFI_USERNAME"
  UNIFI_PASSWORD: "$UNIFI_PASSWORD"
EOF

# Add optional Telegram config if present
if [ -n "$TELEGRAM_BOT_TOKEN" ] && [ -n "$TELEGRAM_CHAT_ID" ]; then
cat <<EOF
  TELEGRAM_BOT_TOKEN: "$TELEGRAM_BOT_TOKEN"
  TELEGRAM_CHAT_ID: "$TELEGRAM_CHAT_ID"
EOF
fi

cat <<EOF

---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: unifi-monitor-data
  namespace: unifi-monitor
spec:
  accessModes:
    - ReadWriteOnce
  resources:
    requests:
      storage: 1Gi

---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: unifi-monitor
  namespace: unifi-monitor
  labels:
    app: unifi-monitor
spec:
  replicas: 1
  selector:
    matchLabels:
      app: unifi-monitor
  template:
    metadata:
      labels:
        app: unifi-monitor
    spec:
      containers:
        - name: unifi-monitor
          image: ghcr.io/cynary/unifi-monitoring:v0.1.2
          ports:
            - containerPort: 8080
              name: http
          envFrom:
            - secretRef:
                name: unifi-monitor-secrets
          env:
            - name: DATABASE_PATH
              value: /data/unifi-monitor.db
            - name: SETUP_TOKEN_PATH
              value: /data/setup-token.txt
            - name: LOG_DIR
              value: /data/logs
            - name: LOG_MAX_SIZE_MB
              value: "${LOG_MAX_SIZE_MB:-512}"
            - name: DB_MAX_SIZE_MB
              value: "${DB_MAX_SIZE_MB:-512}"
            - name: STATIC_DIR
              value: /app/static
          volumeMounts:
            - name: data
              mountPath: /data
          resources:
            requests:
              memory: "64Mi"
              cpu: "100m"
            limits:
              memory: "256Mi"
              cpu: "500m"
          livenessProbe:
            httpGet:
              path: /api/health
              port: http
            initialDelaySeconds: 10
            periodSeconds: 30
          readinessProbe:
            httpGet:
              path: /api/health
              port: http
            initialDelaySeconds: 5
            periodSeconds: 10
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: unifi-monitor-data

---
apiVersion: v1
kind: Service
metadata:
  name: unifi-monitor
  namespace: unifi-monitor
spec:
  type: LoadBalancer
  selector:
    app: unifi-monitor
  ports:
    - port: ${LISTEN_PORT:-8080}
      targetPort: http
      name: http
EOF
