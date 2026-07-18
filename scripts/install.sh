#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo build --manifest-path "$root/Cargo.toml" --release

install_root=${CONTEXT_GUARDIAN_HOME:-${HOME:?HOME is required}/.local/share/context-guardian}
bin_dir=${CONTEXT_GUARDIAN_BIN_DIR:-${HOME}/.local/bin}
mkdir -p "$install_root" "$bin_dir"
cp "$root/target/release/context-guardian" "$install_root/context-guardian"
cp "$root/mcp/server.mjs" "$install_root/context-guardian-mcp.mjs"
cp "$root/scripts/service.sh" "$install_root/service.sh"
chmod +x "$install_root/context-guardian" "$install_root/context-guardian-mcp.mjs" "$install_root/service.sh"
ln -sf "$install_root/context-guardian" "$bin_dir/context-guardian"
cat > "$install_root/context-guardian-mcp" <<EOF
#!/bin/sh
CONTEXT_GUARDIAN_INSTALLED=1 CONTEXT_GUARDIAN_SERVICE_SCRIPT="$install_root/service.sh" exec node "$install_root/context-guardian-mcp.mjs" "\$@"
EOF
chmod +x "$install_root/context-guardian-mcp"
ln -sf "$install_root/context-guardian-mcp" "$bin_dir/context-guardian-mcp"

printf '%s\n' "Installed CLI: $bin_dir/context-guardian" "Installed MCP: $bin_dir/context-guardian-mcp" "Add $bin_dir to PATH if needed."
