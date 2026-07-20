#!/bin/sh
set -eu

action=${1:-}
install_root=${CONTEXT_GUARDIAN_HOME:-${HOME:?HOME is required}/.local/share/context-guardian}
binary=${CONTEXT_GUARDIAN_PASSIVE_CAPTURE_BIN:-$install_root/context-guardian-passive-capture}
codex_root=${CODEX_HOME:-$HOME/.codex}
report_dir=${CONTEXT_GUARDIAN_PASSIVE_CAPTURE_REPORT_DIR:-$codex_root/context-guardian/passive-capture-reports}
capture_interface=${CONTEXT_GUARDIAN_PASSIVE_CAPTURE_INTERFACE:-lo0}
capture_port=${CONTEXT_GUARDIAN_PASSIVE_CAPTURE_PORT:-15721}
capture_seconds=${CONTEXT_GUARDIAN_PASSIVE_CAPTURE_SECONDS:-60}
capture_bytes=${CONTEXT_GUARDIAN_PASSIVE_CAPTURE_MAX_BYTES:-16777216}
max_reports=${CONTEXT_GUARDIAN_PASSIVE_CAPTURE_MAX_REPORTS:-100}
log_dir="$codex_root/context-guardian/logs"
label=com.shangtools.context-guardian-passive-capture

xml_escape() {
  printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'
}

case "$capture_interface" in *[!A-Za-z0-9_.-]*|'') echo "invalid capture interface" >&2; exit 2 ;; esac
case "$capture_port" in *[!0-9]*|'') echo "invalid capture port" >&2; exit 2 ;; esac
case "$capture_seconds" in *[!0-9]*|'') echo "invalid capture duration" >&2; exit 2 ;; esac
case "$capture_bytes" in *[!0-9]*|'') echo "invalid capture size" >&2; exit 2 ;; esac
case "$max_reports" in *[!0-9]*|'') echo "invalid report retention" >&2; exit 2 ;; esac

case "$(uname -s)" in
  Darwin)
    plist="$HOME/Library/LaunchAgents/$label.plist"
    case "$action" in
      install)
        [ -x "$binary" ] || { echo "missing $binary; run scripts/install.sh first" >&2; exit 2; }
        [ -x /usr/sbin/tcpdump ] || { echo "missing /usr/sbin/tcpdump" >&2; exit 2; }
        /usr/sbin/tcpdump -D >/dev/null
        mkdir -p "$HOME/Library/LaunchAgents" "$report_dir" "$log_dir"
        chmod 700 "$report_dir" "$log_dir"
        escaped_binary=$(xml_escape "$binary")
        escaped_interface=$(xml_escape "$capture_interface")
        escaped_report_dir=$(xml_escape "$report_dir")
        escaped_log_dir=$(xml_escape "$log_dir")
        temp_file=$(mktemp)
        trap 'rm -f "$temp_file"' EXIT HUP INT TERM
        printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict>' "<key>Label</key><string>$label</string>" '<key>ProgramArguments</key><array>' "<string>$escaped_binary</string>" '<string>--watch</string>' '<string>--interface</string>' "<string>$escaped_interface</string>" '<string>--port</string>' "<string>$capture_port</string>" '<string>--duration-seconds</string>' "<string>$capture_seconds</string>" '<string>--max-pcap-bytes</string>' "<string>$capture_bytes</string>" '<string>--max-reports</string>' "<string>$max_reports</string>" '<string>--report-dir</string>' "<string>$escaped_report_dir</string>" '<string>--tcpdump</string>' '<string>/usr/sbin/tcpdump</string>' '</array>' '<key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>10</integer>' "<key>StandardOutPath</key><string>$escaped_log_dir/passive-capture.out.log</string>" "<key>StandardErrorPath</key><string>$escaped_log_dir/passive-capture.err.log</string>" '</dict></plist>' > "$temp_file"
        plutil -lint "$temp_file" >/dev/null
        if [ "${CONTEXT_GUARDIAN_DRY_RUN:-0}" = 1 ]; then
          echo "validated passive capture sidecar configuration"
          exit 0
        fi
        install -m 600 "$temp_file" "$plist"
        launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
        launchctl bootstrap "gui/$(id -u)" "$plist"
        echo "installed passive capture sidecar on $capture_interface:$capture_port"
        ;;
      remove)
        launchctl bootout "gui/$(id -u)/$label" >/dev/null 2>&1 || true
        rm -f "$plist"
        echo "removed $label; existing schema-only reports were retained"
        ;;
      status) launchctl print "gui/$(id -u)/$label" ;;
      *) echo "usage: passive-capture-service.sh install | remove | status" >&2; exit 2 ;;
    esac
    ;;
  *) echo "managed passive capture currently supports macOS; run the sidecar binary directly on this platform" >&2; exit 3 ;;
esac
