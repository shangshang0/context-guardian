# Context Relay Server

[English](README.md) | [简体中文](README.zh-CN.md)

Self-host the same multi-tenant Relay used by Context Guardian's default public service.

## Requirements

- A Linux server with Docker Engine and Compose v2.
- A public domain whose A/AAAA record points to the server.
- Inbound TCP port `80` open for ACME, plus `5003` and/or `5004` for HTTPS.

## Deploy

```sh
cp .env.example .env
# Edit only the domain and ACME contact email.
docker compose up -d --build
curl -fsS -o /dev/null -w '%{http_code}\n' https://relay.example.com:5003/healthz
```

The expected health status is `204`. Caddy obtains and renews the certificate automatically. The Relay container is non-root, read-only, capability-free, resource-limited, and has no host mounts. The HTTPS container drops all capabilities except `NET_BIND_SERVICE`, which is required for its internal ports 80/443. Image bytes and tenant registrations are memory-only by default.

Point clients at the self-hosted service during installation:

```sh
CONTEXT_RELAY_URL=https://relay.example.com:5003 ./scripts/install.sh
```

To install binaries without any Relay client, set `CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY=1`.

## Security boundaries

- Clients generate independent secrets locally; the server receives the secret during TLS registration and keeps only a SHA-256 hash in memory.
- Tenant IDs are derived from secrets and registration requires a lightweight proof of work.
- Cross-tenant credentials and path scans receive the same `404` response.
- The Relay does not store images, but its operator can observe transient image bytes and traffic metadata.
- Do not expose the internal Relay port `8080`; only Caddy joins its Docker network.

For persistent tenant hashes, set `CONTEXT_RELAY_TENANT_STORE` and provide a private writable mount owned by UID/GID `65532`. This is optional because clients re-register automatically after restart.
