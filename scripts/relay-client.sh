#!/bin/sh
set -eu

action=${1:-}
relay_url=${2:-${CONTEXT_RELAY_URL:-}}
install_root=${CONTEXT_GUARDIAN_HOME:-${HOME:?HOME is required}/.local/share/context-guardian}
binary="$install_root/context-relay-client"
codex_home=${CODEX_HOME:-$HOME/.codex}
identity_file="$codex_home/context-guardian/relay-identity.json"
log_dir="$codex_home/context-guardian/logs"
label=com.shangtools.context-relay-client

xml_escape() {
  printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'
}

case "$relay_url" in
  https://*) ;;
  *) if [ "$action" = install ]; then echo "relay URL must use https://" >&2; exit 2; fi ;;
esac

case "$(uname -s)" in
  Darwin)
    plist="$HOME/Library/LaunchAgents/$label.plist"
    case "$action" in
      install)
        [ -x "$binary" ] || { echo "missing $binary; run scripts/install.sh first" >&2; exit 2; }
        mkdir -p "$HOME/Library/LaunchAgents" "$log_dir" "$(dirname "$identity_file")"
        chmod 700 "$log_dir" "$(dirname "$identity_file")"
        CONTEXT_RELAY_URL="$relay_url" CONTEXT_RELAY_IDENTITY_FILE="$identity_file" "$binary" --init >/dev/null
        chmod 600 "$identity_file"
        escaped_url=$(xml_escape "$relay_url")
        escaped_binary=$(xml_escape "$binary")
        escaped_identity=$(xml_escape "$identity_file")
        escaped_log=$(xml_escape "$log_dir")
        temp_file=$(mktemp)
        trap 'rm -f "$temp_file"' EXIT HUP INT TERM
        printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict>' "<key>Label</key><string>$label</string>" '<key>ProgramArguments</key><array>' "<string>$escaped_binary</string>" '</array>' '<key>EnvironmentVariables</key><dict>' "<key>CONTEXT_RELAY_URL</key><string>$escaped_url</string>" "<key>CONTEXT_RELAY_IDENTITY_FILE</key><string>$escaped_identity</string>" '<key>CONTEXT_RELAY_LOCAL_GATEWAY</key><string>http://[::1]:8787</string>' '</dict>' '<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>' "<key>StandardOutPath</key><string>$escaped_log/relay-client.out.log</string>" "<key>StandardErrorPath</key><string>$escaped_log/relay-client.err.log</string>" '</dict></plist>' > "$temp_file"
        plutil -lint "$temp_file" >/dev/null
        install -m 600 "$temp_file" "$plist"
        if [ "${CONTEXT_GUARDIAN_DRY_RUN:-0}" = 1 ]; then
          echo "validated relay client configuration"
          exit 0
        fi
        launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
        launchctl bootstrap "gui/$(id -u)" "$plist"
        echo "installed relay client; identity is generated automatically on first start"
        ;;
      remove) launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true; rm -f "$plist"; echo "removed $label" ;;
      status) launchctl print "gui/$(id -u)/$label" ;;
      *) echo "usage: relay-client.sh install RELAY_HTTPS_URL | remove | status" >&2; exit 2 ;;
    esac
    ;;
  *) echo "managed relay client currently supports macOS" >&2; exit 3 ;;
esac
