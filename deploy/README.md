# TrueNAS Scale Deployment

Two options for deploying to TrueNAS Scale:

## Option 1: Custom App (GUI - Easier)

1. Go to **Apps** → **Discover Apps** → **Custom App**

2. Fill in:
   - **Application Name**: `unifi-monitor`
   - **Image Repository**: `ghcr.io/cynary/unifi-monitoring`
   - **Image Tag**: `v0.1.2`

3. Add **Environment Variables**:
   | Name | Value |
   |------|-------|
   | `UNIFI_HOST` | Your UniFi IP (e.g., `192.168.1.1`) |
   | `UNIFI_USERNAME` | Local admin username |
   | `UNIFI_PASSWORD` | Local admin password |
   | `TELEGRAM_BOT_TOKEN` | (Optional) Bot token |
   | `TELEGRAM_CHAT_ID` | (Optional) Chat ID |

4. Add **Storage**:
   - Host Path: `/mnt/your-pool/apps/unifi-monitor`
   - Mount Path: `/data`

5. Add **Port**:
   - Container Port: `8080`
   - Node Port: `8080` (or your preferred port)

6. Click **Install**

## Option 2: Kubernetes YAML (CLI)

1. SSH into your TrueNAS Scale system

2. Edit `truenas-deployment.yaml`:
   - Update the Secret with your credentials
   - Adjust storage class if needed

3. Apply:
   ```bash
   kubectl apply -f truenas-deployment.yaml
   ```

4. Check status:
   ```bash
   kubectl get pods -n unifi-monitor
   kubectl logs -n unifi-monitor deployment/unifi-monitor
   ```

## After Deployment

1. Get the setup token from logs:
   ```bash
   # GUI: Apps → unifi-monitor → Logs
   # CLI: kubectl logs -n unifi-monitor deployment/unifi-monitor | grep "Setup token"
   ```

2. Open `http://your-truenas-ip:8080`

3. Enter setup token and register your passkey

## Updating

To update to a new version:

**GUI**: Apps → unifi-monitor → Edit → Change image tag → Save

**CLI**:
```bash
kubectl set image deployment/unifi-monitor \
  unifi-monitor=ghcr.io/cynary/unifi-monitoring:NEW_VERSION \
  -n unifi-monitor
```
