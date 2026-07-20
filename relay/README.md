# Context Relay Server

[English](README.md) | [简体中文](README.zh-CN.md)

Self-host the same multi-tenant Relay used by Context Guardian's default public service. Compatible v1 terminates image HTTPS at the Relay; preview v2 blindly routes inner TLS to the user's local gateway.

## Requirements

- A Linux server with Docker Engine and Compose v2.
- A public domain whose A/AAAA record points to the server.
- Inbound TCP port `80` open for Caddy ACME, plus `5003` and/or `5004` for the control HTTPS endpoint.
- For v2, inbound TCP `443` and wildcard DNS resolving `*.<blind_suffix>` to this server.

## Deploy

```sh
cp .env.example .env
# Set the control domain, ACME contact email, and blind DNS suffix.
docker compose up -d --build
curl -fsS -o /dev/null -w '%{http_code}\n' https://relay.example.com:5003/healthz
```

The expected health status is `204`. Caddy obtains and renews only the control endpoint certificate. The Relay container is non-root, read-only, capability-free, resource-limited, and has no host mounts. The HTTPS container drops all capabilities except `NET_BIND_SERVICE`, which is required for its internal ports 80/443. Image bytes and tenant registrations are memory-only by default.

The two transports are independent and can coexist:

| Mode | Public data port | Relay visibility | Local gateway |
| --- | --- | --- | --- |
| v1 compatible | `5003`/`5004` | Signed URL, headers, transient image bytes, metadata | HTTP `[::1]:8787` |
| v2 preview | `443` | SNI, IP/timing, ciphertext size | TLS `127.0.0.1:8788` |

For v2, configure:

```dotenv
CONTEXT_RELAY_BLIND_LISTEN=0.0.0.0:8443
CONTEXT_RELAY_BLIND_SUFFIX=relay.example.com
```

Compose maps host `443` directly to the unprivileged Relay listener on container port `8443`. The server accepts only an exact 32-hex tenant label below the configured suffix, reads at most a bounded ClientHello to select that tenant, and never terminates the inner TLS. Do not put an HTTP reverse proxy in front of port `443`; it must carry the original TCP/TLS connection. The WSS control tunnel still uses Caddy on `5003` or `5004` at `/v2/tunnel/<tenant_id>`.

Point clients at the self-hosted service during installation:

```sh
CONTEXT_RELAY_URL=https://relay.example.com:5003 ./scripts/install.sh
```

To install binaries without any Relay client, set `CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY=1`.

## Configure a v2 client

The tenant hostname is `<tenant_id>.<blind_suffix>`. The local setup script verifies the hostname, certificate validity, certificate/private-key match, and mode `0400`/`0600` private-key permissions before starting anything.

Use a matching existing certificate and key:

```sh
CONTEXT_RELAY_BLIND_CERT_FILE=/absolute/path/fullchain.pem \
CONTEXT_RELAY_BLIND_KEY_FILE=/absolute/path/private-key.pem \
./scripts/setup-blind-relay.sh install \
  https://relay.example.com:5003 relay.example.com
```

Or install `acme.sh` locally and issue an exact certificate through TLS-ALPN-01:

```sh
./scripts/setup-blind-relay.sh install \
  https://relay.example.com:5003 relay.example.com admin@example.com
```

During issuance, the script temporarily routes the blind tunnel to `acme.sh --alpn --tlsport 8789`; the permanent service routes to the local TLS gateway on `8788`. The certificate private key never passes through the Relay. Renewal is explicit because the temporary tunnel must be running:

```sh
./scripts/setup-blind-relay.sh renew \
  https://relay.example.com:5003 relay.example.com admin@example.com
```

Use `status` to inspect both launchd services. `remove` stops v2 and deletes its service/publication configuration while retaining certificates, identity, signing key, and image cache. To restore v1 in the same operation, supply its control URL: `./scripts/setup-blind-relay.sh remove https://relay.example.com:5003`.

## Security boundaries

- Clients generate independent secrets locally; the server receives the secret during TLS registration and keeps only a SHA-256 hash in memory.
- Tenant IDs are derived from secrets and registration requires a lightweight proof of work.
- Cross-tenant credentials and path scans receive the same `404` response.
- In v1, the Relay does not store images, but its operator can observe transient image bytes and traffic metadata.
- In v2, the Relay sees SNI, peer addresses, timing, and ciphertext sizes, but not the URL, HMAC signature, HTTP headers, or image plaintext.
- A certificate issued under a shared operator-owned suffix protects against passive and honest-but-curious operation, not a malicious domain owner: that owner can issue an alternate valid certificate and actively MITM a future connection.
- For the strongest boundary, run a dedicated Relay with a client-owned DNS suffix/certificate. The configured server suffix must match the tenant certificate hostname.
- Do not expose the internal Relay port `8080`; only Caddy joins its Docker network.
- Keep the v2 `8443` container listener reachable only through the explicit host `443` mapping.

For persistent tenant hashes, set `CONTEXT_RELAY_TENANT_STORE` and provide a private writable mount owned by UID/GID `65532`. This is optional because clients re-register automatically after restart.
