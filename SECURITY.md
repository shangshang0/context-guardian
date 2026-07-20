# Security

Context Guardian intentionally mutates local Codex state only after an exact thread ID is supplied. Rollout paths must contain that ID, and database filenames must match the expected Codex indexes.

Before recovery, close or pause sensitive tasks when practical. The daemon can handle stale counter writeback, but simultaneous writers always increase operational risk.

Do not enable external summarization unless you trust the configured endpoint with the tool output being summarized.

Image URL publishing is opt-in per guarded task. The public Relay isolates tenants with independent client-generated secrets and does not persist image bytes. Compatible v1 terminates HTTPS at the Relay, so its operator can observe transient image bytes and traffic metadata. Self-host v1 for sensitive images.

Preview blind Relay v2 terminates the image HTTPS connection only in the local TLS gateway. The public Relay parses only the bounded ClientHello SNI needed to select an exact tenant and forwards the remaining TLS records as opaque bytes over an authenticated WSS tunnel. It can still observe the tenant hostname/SNI, source and tunnel IPs, connection timing, and ciphertext sizes. It cannot passively read the signed URL, HMAC, HTTP headers, or image plaintext.

The v2 certificate trust boundary depends on DNS ownership. A locally generated private key under an operator-owned shared suffix prevents passive observation, but the domain owner can issue another publicly trusted certificate and actively terminate a future connection. This is an honest-but-curious, not malicious-operator, guarantee. For the strongest separation, use a dedicated/self-hosted Relay whose configured blind suffix and certificate hostname are controlled by the client. Certificate keys must remain mode `0400` or `0600`; the setup script verifies validity, hostname, key pairing, and permissions. Automatic ACME renewal is explicit because the local TLS-ALPN tunnel must be present during validation.

Relay identities and image signing keys must remain local mode-`0600` files. Do not copy them between users. Report suspected credential exposure promptly and delete the local identity to rotate it after the old tenant is removed or the Relay state is reset. The v2 Relay enforces bounded waiting slots, concurrent connections, ClientHello size/read time, and tunnel lifetime, but operators should still rate-limit and monitor the public TCP `443` listener without logging sensitive tenant-level metadata.

Report vulnerabilities privately through GitHub Security Advisories for `shangshang0/context-guardian`.

The passive-capture preview never changes Codex or provider routing. Raw PCAP files necessarily contain plaintext loopback request data while a capture window is active; they are therefore created mode `0600`, bounded, processed locally, and deleted immediately after schema extraction. Schema-only reports are mode `0600` and omit all header values, bodies, message scalar values, and raw identifiers. Treat packet-capture privileges as sensitive and grant only the minimum OS capability required.

TLS session secrets are sensitive credentials. Guardian does not inject key logging into, restart, or reconfigure Codex or provider processes. If a trusted upstream component already exports session secrets, protect the key log as mode `0600`, rotate or delete it after diagnosis, and remember that sessions completed without exported secrets cannot be decrypted later.
