#!/bin/sh
set -eu

action=${1:-}
ssh_alias=${2:-}
remote_port=${3:-}
local_port=${4:-28787}

case "$ssh_alias" in
  ''|*[!A-Za-z0-9._-]*|*[.@:]*)
    echo "SSH_ALIAS must be a plain Host alias from ~/.ssh/config" >&2
    exit 2
    ;;
esac
case "$remote_port:$local_port" in
  *[!0-9:]*|:*|*:) echo "ports must be numeric" >&2; exit 2 ;;
esac
if [ "$remote_port" -lt 1024 ] || [ "$remote_port" -gt 65535 ] || [ "$local_port" -lt 1024 ] || [ "$local_port" -gt 65535 ]; then
  echo "ports must be between 1024 and 65535" >&2
  exit 2
fi

safe_alias=$(printf '%s' "$ssh_alias" | tr '.-' '__')
label="com.shangtools.context-image-tunnel.$safe_alias.$remote_port"
log_dir="${CODEX_HOME:-${HOME:?HOME is required}/.codex}/context-guardian/logs"
mkdir -p "$log_dir"
chmod 700 "$log_dir"

case "$(uname -s)" in
  Darwin)
    plist="$HOME/Library/LaunchAgents/$label.plist"
    case "$action" in
      install)
        ssh -G "$ssh_alias" >/dev/null 2>&1 || { echo "unknown SSH alias: $ssh_alias" >&2; exit 2; }
        ssh -o BatchMode=yes -o ConnectTimeout=10 "$ssh_alias" true
        mkdir -p "$HOME/Library/LaunchAgents"
        temp_file=$(mktemp)
        trap 'rm -f "$temp_file"' EXIT HUP INT TERM
        printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict>' "<key>Label</key><string>$label</string>" '<key>ProgramArguments</key><array>' '<string>/usr/bin/ssh</string>' '<string>-NT</string>' '<string>-o</string><string>BatchMode=yes</string>' '<string>-o</string><string>ExitOnForwardFailure=yes</string>' '<string>-o</string><string>ServerAliveInterval=15</string>' '<string>-o</string><string>ServerAliveCountMax=3</string>' '<string>-o</string><string>ConnectTimeout=10</string>' '<string>-R</string>' "<string>0.0.0.0:$remote_port:127.0.0.1:$local_port</string>" "<string>$ssh_alias</string>" '</array>' '<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>' "<key>StandardOutPath</key><string>$log_dir/$safe_alias-$remote_port-tunnel.out.log</string>" "<key>StandardErrorPath</key><string>$log_dir/$safe_alias-$remote_port-tunnel.err.log</string>" '</dict></plist>' > "$temp_file"
        install -m 600 "$temp_file" "$plist"
        launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
        launchctl bootstrap "gui/$(id -u)" "$plist"
        echo "installed image tunnel via SSH alias $ssh_alias on remote TCP $remote_port"
        ;;
      remove)
        launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
        rm -f "$plist"
        echo "removed $label"
        ;;
      status) launchctl print "gui/$(id -u)/$label" ;;
      *) echo "usage: image-tunnel.sh install|remove|status SSH_ALIAS REMOTE_PORT [LOCAL_PORT]" >&2; exit 2 ;;
    esac
    ;;
  *) echo "managed image tunnels currently support macOS; run ssh -NT -R manually on this OS" >&2; exit 3 ;;
esac
