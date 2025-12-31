# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in UniFi Monitor, please report it responsibly:

1. **Do not** open a public GitHub issue for security vulnerabilities
2. Email the maintainer directly or use GitHub's private vulnerability reporting feature
3. Include a detailed description of the vulnerability and steps to reproduce

## Security Best Practices

When deploying UniFi Monitor:

### Credentials
- Never commit `.env` files or credentials to version control
- Use strong, unique passwords for the UniFi local account
- Rotate credentials periodically

### Network Security
- Run behind a reverse proxy with HTTPS in production
- Set `RP_ORIGIN` to your actual domain (enables secure cookies)
- Consider restricting access to trusted networks

### Authentication
- UniFi Monitor uses passkey (WebAuthn) authentication - no passwords to leak
- Setup tokens are single-use and should be deleted after initial setup
- Invite tokens expire after 5 minutes by default

### Data Storage
- The SQLite database contains event data and passkey credentials
- Ensure the `/data` volume has appropriate filesystem permissions
- Back up regularly but store backups securely

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| latest  | :white_check_mark: |

## Security Features

- **Passkey-only authentication**: Phishing-resistant, no passwords
- **Rate limiting**: Auth endpoints are rate-limited to prevent brute force
- **Session management**: Configurable expiry, secure cookies over HTTPS
- **CORS protection**: Configurable allowed origins
- **No secrets in images**: All credentials are runtime environment variables
