#!/bin/sh
set -eu

action=${1:-}
relay_url=${2:-${CONTEXT_RELAY_URL:-}}
blind_suffix=${3:-${CONTEXT_RELAY_BLIND_SUFFIX:-}}
acme_email=${4:-${CONTEXT_RELAY_ACME_EMAIL:-}}
install_root=${CONTEXT_GUARDIAN_HOME:-${HOME:?HOME is required}/.local/share/context-guardian}
codex_root=${CODEX_HOME:-$HOME/.codex}
secure_dir="$codex_root/context-guardian"
blind_dir="$secure_dir/blind-tls"
cache_dir="$secure_dir/images"
signing_key="$secure_dir/image-signing.key"
identity_file="$secure_dir/relay-identity.json"
publishing_config="$secure_dir/image-publishing.env"
blind_config="$blind_dir/config.env"
relay_binary="$install_root/context-relay-client"
gateway_binary="$install_root/context-image-gateway"
relay_service="$install_root/relay-client.sh"
public_relay_setup="$install_root/setup-public-relay.sh"
gateway_label=com.shangtools.context-image-gateway-blind
gateway_plist="$HOME/Library/LaunchAgents/$gateway_label.plist"
relay_plist="$HOME/Library/LaunchAgents/com.shangtools.context-relay-client.plist"
log_dir="$secure_dir/logs"
tunnel_pid=
restore_relay=0

xml_escape() {
  printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'
}

validate_inputs() {
  case "$relay_url" in https://*) ;; *) echo "relay control URL must use HTTPS" >&2; exit 2 ;; esac
  valid_dns_suffix "$blind_suffix" || { echo "invalid blind relay DNS suffix" >&2; exit 2; }
  [ -x "$relay_binary" ] || { echo "missing $relay_binary; run scripts/install.sh first" >&2; exit 2; }
  [ -x "$gateway_binary" ] || { echo "missing $gateway_binary; run scripts/install.sh first" >&2; exit 2; }
  [ -x "$relay_service" ] || { echo "missing $relay_service; run scripts/install.sh first" >&2; exit 2; }
}

valid_dns_suffix() {
  [ -n "$1" ] && [ "${#1}" -le 253 ] || return 1
  case "$1" in .*|*.|*..*) return 1 ;; esac
  suffix_rest=$1
  while [ -n "$suffix_rest" ]; do
    case "$suffix_rest" in
      *.*) label=${suffix_rest%%.*}; suffix_rest=${suffix_rest#*.} ;;
      *) label=$suffix_rest; suffix_rest= ;;
    esac
    case "$label" in ''|-*|*-|*[!A-Za-z0-9-]*) return 1 ;; esac
    [ "${#label}" -le 63 ] || return 1
  done
}

initialize_material() {
  mkdir -p "$secure_dir" "$blind_dir" "$cache_dir" "$log_dir" "$HOME/Library/LaunchAgents"
  chmod 700 "$secure_dir" "$blind_dir" "$cache_dir" "$log_dir"
  if [ ! -s "$signing_key" ]; then
    openssl rand -out "$signing_key" 32
  fi
  chmod 600 "$signing_key"
  tenant_id=$(CONTEXT_RELAY_URL="$relay_url" CONTEXT_RELAY_IDENTITY_FILE="$identity_file" "$relay_binary" --init)
  case "$tenant_id" in ''|*[!0-9a-f]*|?????????????????????????????????*) echo "invalid relay tenant ID" >&2; exit 2 ;; esac
  [ "${#tenant_id}" -eq 32 ] || { echo "invalid relay tenant ID length" >&2; exit 2; }
  blind_hostname="$tenant_id.$blind_suffix"
}

find_acme() {
  if [ -n "${CONTEXT_RELAY_ACME_SH:-}" ]; then
    acme_command=$CONTEXT_RELAY_ACME_SH
  elif command -v acme.sh >/dev/null 2>&1; then
    acme_command=$(command -v acme.sh)
  elif [ -x "$HOME/.acme.sh/acme.sh" ]; then
    acme_command="$HOME/.acme.sh/acme.sh"
  else
    echo "acme.sh is required for automatic local certificate issuance; alternatively set CONTEXT_RELAY_BLIND_CERT_FILE and CONTEXT_RELAY_BLIND_KEY_FILE" >&2
    exit 2
  fi
  [ -x "$acme_command" ] || { echo "acme.sh executable is not usable: $acme_command" >&2; exit 2; }
  [ -n "$acme_email" ] || { echo "ACME email is required" >&2; exit 2; }
}

cleanup_acme_tunnel() {
  trap - EXIT HUP INT TERM
  if [ -n "$tunnel_pid" ]; then
    kill "$tunnel_pid" >/dev/null 2>&1 || true
    wait "$tunnel_pid" >/dev/null 2>&1 || true
    tunnel_pid=
  fi
  if [ "$restore_relay" = 1 ] && [ -f "$relay_plist" ]; then
    launchctl bootstrap "gui/$(id -u)" "$relay_plist" >/dev/null 2>&1 || true
  fi
}

issue_certificate() {
  find_acme
  cert_file="$blind_dir/fullchain.pem"
  tls_key_file="$blind_dir/private-key.pem"
  if [ -f "$relay_plist" ]; then
    restore_relay=1
  fi
  launchctl bootout "gui/$(id -u)/com.shangtools.context-relay-client" >/dev/null 2>&1 || true
  tunnel_log="$blind_dir/acme-tunnel.log"
  CONTEXT_RELAY_URL="$relay_url" \
  CONTEXT_RELAY_IDENTITY_FILE="$identity_file" \
  CONTEXT_RELAY_BLIND_GATEWAY=127.0.0.1:8789 \
  CONTEXT_RELAY_BLIND_SLOTS=4 \
  CONTEXT_RELAY_BLIND_ONLY=1 \
  "$relay_binary" >"$tunnel_log" 2>&1 &
  tunnel_pid=$!
  trap cleanup_acme_tunnel EXIT
  trap 'cleanup_acme_tunnel; exit 1' HUP INT TERM
  attempts=0
  while ! grep -q 'blind TLS slots=' "$tunnel_log"; do
    kill -0 "$tunnel_pid" >/dev/null 2>&1 || { echo "temporary blind tunnel exited" >&2; exit 1; }
    attempts=$((attempts + 1))
    [ "$attempts" -lt 80 ] || { echo "timed out preparing blind ACME tunnel" >&2; exit 1; }
    sleep 0.25
  done
  if [ "$action" = renew ]; then
    "$acme_command" --issue --force --alpn --tlsport 8789 --server letsencrypt --accountemail "$acme_email" -d "$blind_hostname"
  else
    "$acme_command" --issue --alpn --tlsport 8789 --server letsencrypt --accountemail "$acme_email" -d "$blind_hostname"
  fi
  "$acme_command" --install-cert -d "$blind_hostname" --key-file "$tls_key_file" --fullchain-file "$cert_file"
  cleanup_acme_tunnel
  chmod 600 "$tls_key_file" "$cert_file"
}

select_certificate() {
  if [ -n "${CONTEXT_RELAY_BLIND_CERT_FILE:-}" ] || [ -n "${CONTEXT_RELAY_BLIND_KEY_FILE:-}" ]; then
    [ -f "${CONTEXT_RELAY_BLIND_CERT_FILE:-}" ] && [ -f "${CONTEXT_RELAY_BLIND_KEY_FILE:-}" ] || { echo "both blind certificate and key files are required" >&2; exit 2; }
    cert_file=$CONTEXT_RELAY_BLIND_CERT_FILE
    tls_key_file=$CONTEXT_RELAY_BLIND_KEY_FILE
  elif [ -f "$blind_dir/fullchain.pem" ] && [ -f "$blind_dir/private-key.pem" ] && [ "$action" != renew ] && openssl x509 -in "$blind_dir/fullchain.pem" -noout -checkend 86400 >/dev/null 2>&1; then
    cert_file="$blind_dir/fullchain.pem"
    tls_key_file="$blind_dir/private-key.pem"
  else
    issue_certificate
  fi
  openssl x509 -in "$cert_file" -noout -checkend 0 >/dev/null
  openssl x509 -in "$cert_file" -noout -checkhost "$blind_hostname" >/dev/null
  key_mode=$(stat -f '%Lp' "$tls_key_file")
  case "$key_mode" in 400|600) ;; *) echo "TLS private key must have mode 0400 or 0600" >&2; exit 2 ;; esac
  cert_public_key=$(openssl x509 -in "$cert_file" -pubkey -noout | openssl pkey -pubin -outform DER | openssl dgst -sha256)
  private_public_key=$(openssl pkey -in "$tls_key_file" -pubout -outform DER | openssl dgst -sha256)
  [ "$cert_public_key" = "$private_public_key" ] || { echo "TLS certificate and private key do not match" >&2; exit 2; }
}

install_gateway() {
  escaped_gateway=$(xml_escape "$gateway_binary")
  escaped_cache=$(xml_escape "$cache_dir")
  escaped_signing_key=$(xml_escape "$signing_key")
  escaped_cert=$(xml_escape "$cert_file")
  escaped_tls_key=$(xml_escape "$tls_key_file")
  escaped_log=$(xml_escape "$log_dir")
  temporary=$(mktemp)
  trap 'rm -f "$temporary"' EXIT HUP INT TERM
  printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict>' "<key>Label</key><string>$gateway_label</string>" '<key>ProgramArguments</key><array>' "<string>$escaped_gateway</string>" '<string>--listen</string><string>127.0.0.1:8788</string>' '<string>--cache-dir</string>' "<string>$escaped_cache</string>" '<string>--signing-key-file</string>' "<string>$escaped_signing_key</string>" '<string>--tls-cert-file</string>' "<string>$escaped_cert</string>" '<string>--tls-key-file</string>' "<string>$escaped_tls_key</string>" '</array>' '<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>' "<key>StandardOutPath</key><string>$escaped_log/blind-gateway.out.log</string>" "<key>StandardErrorPath</key><string>$escaped_log/blind-gateway.err.log</string>" '</dict></plist>' >"$temporary"
  plutil -lint "$temporary" >/dev/null
  if [ "${CONTEXT_GUARDIAN_DRY_RUN:-0}" = 1 ]; then
    trap - EXIT HUP INT TERM
    rm -f "$temporary"
    return
  fi
  install -m 600 "$temporary" "$gateway_plist"
  launchctl bootout "gui/$(id -u)/$gateway_label" >/dev/null 2>&1 || true
  launchctl bootstrap "gui/$(id -u)" "$gateway_plist"
  trap - EXIT HUP INT TERM
  rm -f "$temporary"
}

install_services() {
  install_gateway
  CONTEXT_RELAY_BLIND_GATEWAY=127.0.0.1:8788 CONTEXT_RELAY_BLIND_SLOTS=4 "$relay_service" install "$relay_url"
  if [ "${CONTEXT_GUARDIAN_DRY_RUN:-0}" = 1 ]; then
    echo "validated blind TLS image relay for $blind_hostname"
    return
  fi
  printf '%s\n' "CONTEXT_RELAY_URL=$relay_url" "CONTEXT_RELAY_BLIND_SUFFIX=$blind_suffix" "CONTEXT_RELAY_BLIND_HOSTNAME=$blind_hostname" "CONTEXT_RELAY_BLIND_CERT_FILE=$cert_file" "CONTEXT_RELAY_BLIND_KEY_FILE=$tls_key_file" >"$blind_config"
  chmod 600 "$blind_config"
  printf '%s\n' "CONTEXT_GUARDIAN_IMAGE_BASE_URL=https://$blind_hostname" "CONTEXT_GUARDIAN_IMAGE_SIGNING_KEY_FILE=$signing_key" "CONTEXT_GUARDIAN_IMAGE_CACHE_DIR=$cache_dir" "CONTEXT_GUARDIAN_IMAGE_URL_TTL_SECONDS=900" >"$publishing_config"
  chmod 600 "$publishing_config"
  echo "installed blind TLS image relay for $blind_hostname"
}

case "$(uname -s)" in Darwin) ;; *) echo "managed blind Relay setup currently supports macOS" >&2; exit 3 ;; esac

case "$action" in
  install|renew)
    validate_inputs
    initialize_material
    select_certificate
    install_services
    ;;
  status)
    [ -f "$blind_config" ] || { echo "blind Relay is not configured" >&2; exit 3; }
    launchctl print "gui/$(id -u)/$gateway_label"
    launchctl print "gui/$(id -u)/com.shangtools.context-relay-client"
    ;;
  remove)
    launchctl bootout "gui/$(id -u)/$gateway_label" >/dev/null 2>&1 || true
    rm -f "$gateway_plist" "$blind_config" "$publishing_config"
    "$relay_service" remove
    if [ -n "$relay_url" ]; then
      [ -x "$public_relay_setup" ] || { echo "missing $public_relay_setup; run scripts/install.sh first" >&2; exit 2; }
      "$public_relay_setup" "$relay_url"
    fi
    echo "removed blind TLS services; certificates, identity, signing key, and image cache were retained; v1 was restored only when a Relay URL was supplied"
    ;;
  *) echo "usage: setup-blind-relay.sh install|renew RELAY_HTTPS_URL BLIND_DNS_SUFFIX ACME_EMAIL | status | remove [RELAY_HTTPS_URL]" >&2; exit 2 ;;
esac
