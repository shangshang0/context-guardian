#!/bin/sh
set -eu

action=${1:-}
thread_id=${2:-}
binary=${3:-}

case "$thread_id" in
  *[!0-9A-Fa-f-]*|'') echo "invalid thread id" >&2; exit 2 ;;
esac

if [ -z "$binary" ]; then
  script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
  binary="$script_dir/../target/release/context-guardian"
fi

codex_home=${CODEX_HOME:-${HOME:?HOME is required}/.codex}
log_dir="$codex_home/context-guardian/logs"
mkdir -p "$log_dir"

case "$(uname -s)" in
  Darwin)
    label="com.shangtools.context-guardian.$thread_id"
    plist="$HOME/Library/LaunchAgents/$label.plist"
    case "$action" in
      install)
        mkdir -p "$HOME/Library/LaunchAgents"
        escaped_binary=$(printf '%s' "$binary" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g')
        escaped_home=$(printf '%s' "$codex_home" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g')
        escaped_log=$(printf '%s' "$log_dir" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g')
        apply_file=$(mktemp)
        trap 'rm -f "$apply_file"' EXIT HUP INT TERM
        printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict>' "<key>Label</key><string>$label</string>" '<key>ProgramArguments</key><array>' "<string>$escaped_binary</string>" '<string>--thread-id</string>' "<string>$thread_id</string>" '</array>' '<key>EnvironmentVariables</key><dict>' "<key>CODEX_HOME</key><string>$escaped_home</string>" '</dict>' '<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>' "<key>StandardOutPath</key><string>$escaped_log/$thread_id.out.log</string>" "<key>StandardErrorPath</key><string>$escaped_log/$thread_id.err.log</string>" '</dict></plist>' > "$apply_file"
        cp "$apply_file" "$plist"
        launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
        launchctl bootstrap "gui/$(id -u)" "$plist"
        echo "installed $label"
        ;;
      remove) launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true; rm -f "$plist"; echo "removed $label" ;;
      status) launchctl print "gui/$(id -u)/$label" ;;
      *) echo "usage: service.sh install|remove|status THREAD_ID [BINARY]" >&2; exit 2 ;;
    esac
    ;;
  Linux)
    unit="context-guardian-$thread_id.service"
    unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    unit_path="$unit_dir/$unit"
    case "$action" in
      install)
        mkdir -p "$unit_dir"
        quoted_binary=$(printf '%s' "$binary" | sed 's/\\/\\\\/g; s/"/\\"/g')
        quoted_home=$(printf '%s' "$codex_home" | sed 's/\\/\\\\/g; s/"/\\"/g')
        printf '%s\n' '[Unit]' "Description=Context Guardian for $thread_id" '[Service]' "Environment=CODEX_HOME=$quoted_home" "ExecStart=$quoted_binary --thread-id $thread_id" 'Restart=always' 'RestartSec=2' '[Install]' 'WantedBy=default.target' > "$unit_path"
        systemctl --user daemon-reload
        systemctl --user enable --now "$unit"
        echo "installed $unit"
        ;;
      remove) systemctl --user disable --now "$unit" 2>/dev/null || true; rm -f "$unit_path"; systemctl --user daemon-reload; echo "removed $unit" ;;
      status) systemctl --user status "$unit" --no-pager ;;
      *) echo "usage: service.sh install|remove|status THREAD_ID [BINARY]" >&2; exit 2 ;;
    esac
    ;;
  *) echo "background service installation supports macOS and Linux; run the binary directly on this OS" >&2; exit 3 ;;
esac
