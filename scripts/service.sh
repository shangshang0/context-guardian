#!/bin/sh
set -eu

action=${1:-}
thread_id=${2:-}
binary=${3:-}

case "$thread_id" in
  *[!0-9A-Fa-f-]*|'') echo "invalid thread id" >&2; exit 2 ;;
esac

if [ -z "$binary" ]; then
  script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
  binary="$script_dir/../target/release/context-guardian"
fi

codex_home=${CODEX_HOME:-${HOME:?HOME is required}/.codex}
log_dir="$codex_home/context-guardian/logs"
mkdir -p "$log_dir"
chmod 700 "$log_dir"

xml_escape() {
  printf '%s' "$1" | sed 's/&/\&amp;/g; s/</\&lt;/g; s/>/\&gt;/g'
}

systemd_quote() {
  escaped=$(printf '%s' "$1" | sed 's/%/%%/g; s/\\/\\\\/g; s/"/\\"/g')
  printf '"%s"' "$escaped"
}

large_output_bytes=${CONTEXT_GUARDIAN_LARGE_TOOL_OUTPUT_BYTES:-160000}
case "$large_output_bytes" in ''|*[!0-9]*) echo "invalid large tool output threshold" >&2; exit 2 ;; esac

cc_switch_enabled=${CONTEXT_GUARDIAN_CC_SWITCH_SUMMARY:-0}
cc_switch_url=${CONTEXT_GUARDIAN_CC_SWITCH_URL:-http://127.0.0.1:15721/v1/chat/completions}
cc_switch_model=${CONTEXT_GUARDIAN_CC_SWITCH_MODEL:-feature/gpt-5.6-sol}
cc_switch_chunk_tokens=${CONTEXT_GUARDIAN_CC_SWITCH_CHUNK_TARGET_TOKENS:-120000}
if [ "$cc_switch_enabled" = 1 ]; then
  case "$cc_switch_url" in http://*|https://*) ;; *) echo "CC Switch URL must use HTTP or HTTPS" >&2; exit 2 ;; esac
  printf '%s' "$cc_switch_url" | LC_ALL=C awk 'BEGIN { valid = 1 } { count += 1 } !/^[[:graph:]]+$/ || length($0) > 2048 { valid = 0 } END { exit !(valid && count == 1) }' || { echo "invalid CC Switch URL" >&2; exit 2; }
  printf '%s' "$cc_switch_model" | LC_ALL=C awk 'BEGIN { valid = 1 } { count += 1 } !/^[[:graph:]]+$/ || length($0) > 200 { valid = 0 } END { exit !(valid && count == 1) }' || { echo "invalid CC Switch model" >&2; exit 2; }
  case "$cc_switch_chunk_tokens" in ''|*[!0-9]*) echo "invalid CC Switch chunk target" >&2; exit 2 ;; esac
fi

case "$(uname -s)" in
  Darwin)
    label="com.shangtools.context-guardian.$thread_id"
    plist="$HOME/Library/LaunchAgents/$label.plist"
    case "$action" in
      install)
        mkdir -p "$HOME/Library/LaunchAgents"
        escaped_binary=$(xml_escape "$binary")
        escaped_home=$(xml_escape "$codex_home")
        escaped_log=$(xml_escape "$log_dir")
        image_config="$codex_home/context-guardian/image-publishing.env"
        image_arguments=''
        preview_arguments=''
        cc_switch_arguments="<string>--large-tool-output-bytes</string><string>$large_output_bytes</string>"
        if [ -f "$image_config" ]; then
          [ "$(stat -f '%Lp' "$image_config")" = 600 ] || { echo "$image_config must have mode 600" >&2; exit 2; }
          image_base_url=$(sed -n 's/^CONTEXT_GUARDIAN_IMAGE_BASE_URL=//p' "$image_config")
          image_key_file=$(sed -n 's/^CONTEXT_GUARDIAN_IMAGE_SIGNING_KEY_FILE=//p' "$image_config")
          image_cache_dir=$(sed -n 's/^CONTEXT_GUARDIAN_IMAGE_CACHE_DIR=//p' "$image_config")
          image_ttl=$(sed -n 's/^CONTEXT_GUARDIAN_IMAGE_URL_TTL_SECONDS=//p' "$image_config")
          case "$image_base_url" in https://*) ;; *) echo "invalid image base URL" >&2; exit 2 ;; esac
          [ -f "$image_key_file" ] || { echo "missing image signing key" >&2; exit 2; }
          [ -d "$image_cache_dir" ] || { echo "missing image cache directory" >&2; exit 2; }
          case "$image_ttl" in ''|*[!0-9]*) echo "invalid image URL TTL" >&2; exit 2 ;; esac
          image_arguments="<string>--image-base-url</string><string>$(xml_escape "$image_base_url")</string><string>--image-signing-key-file</string><string>$(xml_escape "$image_key_file")</string><string>--image-cache-dir</string><string>$(xml_escape "$image_cache_dir")</string><string>--image-url-ttl-seconds</string><string>$image_ttl</string>"
        fi
        if [ "${CONTEXT_GUARDIAN_MESSAGE_FORMAT_PREVIEW:-0}" = 1 ]; then
          preview_arguments='<string>--enable-message-format-preview</string>'
          if [ "${CONTEXT_GUARDIAN_MESSAGE_FORMAT_LIVE_PROBE:-0}" = 1 ]; then
            preview_arguments="$preview_arguments<string>--enable-message-format-live-probe</string>"
          fi
          if [ "${CONTEXT_GUARDIAN_MESSAGE_FORMAT_PASSIVE_CAPTURE:-0}" = 1 ]; then
            preview_arguments="$preview_arguments<string>--enable-message-format-passive-capture</string>"
          fi
        fi
        if [ "$cc_switch_enabled" = 1 ]; then
          cc_switch_arguments="$cc_switch_arguments<string>--enable-cc-switch-summary</string><string>--cc-switch-url</string><string>$(xml_escape "$cc_switch_url")</string><string>--cc-switch-model</string><string>$(xml_escape "$cc_switch_model")</string><string>--cc-switch-chunk-target-tokens</string><string>$cc_switch_chunk_tokens</string>"
        fi
        apply_file=$(mktemp)
        trap 'rm -f "$apply_file"' EXIT HUP INT TERM
        printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' '<plist version="1.0"><dict>' "<key>Label</key><string>$label</string>" '<key>ProgramArguments</key><array>' "<string>$escaped_binary</string>" '<string>--thread-id</string>' "<string>$thread_id</string>" "$image_arguments" "$cc_switch_arguments" "$preview_arguments" '</array>' '<key>EnvironmentVariables</key><dict>' "<key>CODEX_HOME</key><string>$escaped_home</string>" '</dict>' '<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>' "<key>StandardOutPath</key><string>$escaped_log/$thread_id.out.log</string>" "<key>StandardErrorPath</key><string>$escaped_log/$thread_id.err.log</string>" '</dict></plist>' > "$apply_file"
        plutil -lint "$apply_file" >/dev/null
        if [ "${CONTEXT_GUARDIAN_DRY_RUN:-0}" = 1 ]; then
          echo "validated $label"
          exit 0
        fi
        install -m 600 "$apply_file" "$plist"
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
        preview_arguments=''
        cc_switch_arguments=" --large-tool-output-bytes $(systemd_quote "$large_output_bytes")"
        if [ "${CONTEXT_GUARDIAN_MESSAGE_FORMAT_PREVIEW:-0}" = 1 ]; then
          preview_arguments=' --enable-message-format-preview'
          if [ "${CONTEXT_GUARDIAN_MESSAGE_FORMAT_LIVE_PROBE:-0}" = 1 ]; then
            preview_arguments="$preview_arguments --enable-message-format-live-probe"
          fi
          if [ "${CONTEXT_GUARDIAN_MESSAGE_FORMAT_PASSIVE_CAPTURE:-0}" = 1 ]; then
            preview_arguments="$preview_arguments --enable-message-format-passive-capture"
          fi
        fi
        if [ "$cc_switch_enabled" = 1 ]; then
          cc_switch_arguments="$cc_switch_arguments --enable-cc-switch-summary --cc-switch-url $(systemd_quote "$cc_switch_url") --cc-switch-model $(systemd_quote "$cc_switch_model") --cc-switch-chunk-target-tokens $(systemd_quote "$cc_switch_chunk_tokens")"
        fi
        printf '%s\n' '[Unit]' "Description=Context Guardian for $thread_id" '[Service]' "Environment=CODEX_HOME=$quoted_home" "ExecStart=$quoted_binary --thread-id $thread_id$cc_switch_arguments$preview_arguments" 'Restart=always' 'RestartSec=2' '[Install]' 'WantedBy=default.target' > "$unit_path"
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
