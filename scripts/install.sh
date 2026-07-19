#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --manifest-path "$root/Cargo.toml" --release
cargo build --manifest-path "$root/relay/Cargo.toml" --release --bin context-relay-client

install_root=${CONTEXT_GUARDIAN_HOME:-${HOME:?HOME is required}/.local/share/context-guardian}
bin_dir=${CONTEXT_GUARDIAN_BIN_DIR:-${HOME}/.local/bin}
mkdir -p "$install_root" "$bin_dir"
cp "$root/target/release/context-guardian" "$install_root/context-guardian"
cp "$root/target/release/context-image-gateway" "$install_root/context-image-gateway"
cp "$root/relay/target/release/context-relay-client" "$install_root/context-relay-client"
cp "$root/mcp/server.mjs" "$install_root/context-guardian-mcp.mjs"
cp "$root/scripts/service.sh" "$install_root/service.sh"
cp "$root/scripts/image-tunnel.sh" "$install_root/image-tunnel.sh"
cp "$root/scripts/relay-client.sh" "$install_root/relay-client.sh"
cp "$root/scripts/setup-public-relay.sh" "$install_root/setup-public-relay.sh"
chmod +x "$install_root/context-guardian" "$install_root/context-image-gateway" "$install_root/context-relay-client" "$install_root/context-guardian-mcp.mjs" "$install_root/service.sh" "$install_root/image-tunnel.sh" "$install_root/relay-client.sh" "$install_root/setup-public-relay.sh"
ln -sf "$install_root/context-guardian" "$bin_dir/context-guardian"
ln -sf "$install_root/context-image-gateway" "$bin_dir/context-image-gateway"
cat > "$install_root/context-guardian-mcp" <<EOF
#!/bin/sh
CONTEXT_GUARDIAN_INSTALLED=1 CONTEXT_GUARDIAN_SERVICE_SCRIPT="$install_root/service.sh" CONTEXT_RELAY_CLIENT_SCRIPT="$install_root/relay-client.sh" exec node "$install_root/context-guardian-mcp.mjs" "\$@"
EOF
chmod +x "$install_root/context-guardian-mcp"
ln -sf "$install_root/context-guardian-mcp" "$bin_dir/context-guardian-mcp"

printf '%s\n' "Installed CLI: $bin_dir/context-guardian" "Installed gateway: $bin_dir/context-image-gateway" "Installed relay client: $install_root/context-relay-client" "Installed tunnel managers: $install_root/image-tunnel.sh and $install_root/relay-client.sh" "Installed MCP: $bin_dir/context-guardian-mcp" "Add $bin_dir to PATH if needed."

if [ "${CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY:-0}" != 1 ] && [ "$(uname -s)" = Darwin ]; then
  "$install_root/setup-public-relay.sh" "${CONTEXT_RELAY_URL:-https://dxcfvghbjdfnaef.duckdns.org:5003}"
fi
