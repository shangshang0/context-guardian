#!/bin/sh
set -eu

relay_url=${1:-${CONTEXT_RELAY_URL:-https://dxcfvghbjdfnaef.duckdns.org:5003}}
install_root=${CONTEXT_GUARDIAN_HOME:-${HOME:?HOME is required}/.local/share/context-guardian}
codex_home=${CODEX_HOME:-$HOME/.codex}
secure_dir="$codex_home/context-guardian"
cache_dir="$secure_dir/images"
key_file="$secure_dir/image-signing.key"
identity_file="$secure_dir/relay-identity.json"
config_file="$secure_dir/image-publishing.env"
gateway_label=com.shangtools.context-image-gateway

case "$relay_url" in https://*) ;; *) echo "relay URL must use https://" >&2; exit 2 ;; esac
[ -x "$install_root/context-image-gateway" ] || { echo "run scripts/install.sh first" >&2; exit 2; }
[ -x "$install_root/context-relay-client" ] || { echo "run scripts/install.sh first" >&2; exit 2; }

mkdir -p "$secure_dir" "$cache_dir" "$HOME/Library/LaunchAgents"
chmod 700 "$secure_dir" "$cache_dir"
if [ ! -s "$key_file" ]; then
  openssl rand 32 > "$key_file"
fi
chmod 600 "$key_file"

"$install_root/relay-client.sh" install "$relay_url"
tenant_id=$(CONTEXT_RELAY_URL="$relay_url" CONTEXT_RELAY_IDENTITY_FILE="$identity_file" "$install_root/context-relay-client" --init)
case "$tenant_id" in *[!0-9a-f]*|'') echo "invalid generated tenant ID" >&2; exit 2 ;; esac
base_url="${relay_url%/}/t/$tenant_id"
printf '%s\n' "CONTEXT_GUARDIAN_IMAGE_BASE_URL=$base_url" "CONTEXT_GUARDIAN_IMAGE_SIGNING_KEY_FILE=$key_file" "CONTEXT_GUARDIAN_IMAGE_CACHE_DIR=$cache_dir" "CONTEXT_GUARDIAN_IMAGE_URL_TTL_SECONDS=900" > "$config_file"
chmod 600 "$config_file"

plist="$HOME/Library/LaunchAgents/$gateway_label.plist"
temp_file=$(mktemp)
trap 'rm -f "$temp_file"' EXIT HUP INT TERM
printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict>' "<key>Label</key><string>$gateway_label</string>" '<key>ProgramArguments</key><array>' "<string>$install_root/context-image-gateway</string>" '<string>--listen</string><string>[::1]:8787</string>' '<string>--cache-dir</string>' "<string>$cache_dir</string>" '<string>--signing-key-file</string>' "<string>$key_file</string>" '</array>' '<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>' "<key>StandardOutPath</key><string>$secure_dir/gateway.out.log</string>" "<key>StandardErrorPath</key><string>$secure_dir/gateway.err.log</string>" '</dict></plist>' > "$temp_file"
install -m 600 "$temp_file" "$plist"
launchctl bootout "gui/$(id -u)/$gateway_label" >/dev/null 2>&1 || true
launchctl bootstrap "gui/$(id -u)" "$plist"
echo "public Relay image support is ready; guardian arguments are stored in $config_file"
