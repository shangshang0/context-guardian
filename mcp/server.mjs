#!/usr/bin/env node
import { spawn } from "node:child_process";
import { access } from "node:fs/promises";
import { constants } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import readline from "node:readline";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const installedSibling = process.platform === "win32"
  ? join(dirname(fileURLToPath(import.meta.url)), "context-guardian.exe")
  : join(dirname(fileURLToPath(import.meta.url)), "context-guardian");
const defaultBinary = process.platform === "win32"
  ? join(root, "target", "release", "context-guardian.exe")
  : join(root, "target", "release", "context-guardian");
const binary = process.env.CONTEXT_GUARDIAN_BIN || (process.env.CONTEXT_GUARDIAN_INSTALLED === "1" ? installedSibling : defaultBinary);
const passiveCaptureBinary = process.env.CONTEXT_GUARDIAN_PASSIVE_CAPTURE_BIN || (process.env.CONTEXT_GUARDIAN_INSTALLED === "1"
  ? join(dirname(fileURLToPath(import.meta.url)), "context-guardian-passive-capture")
  : join(root, "target", "release", "context-guardian-passive-capture"));
const serviceScript = process.env.CONTEXT_GUARDIAN_SERVICE_SCRIPT || join(root, "scripts", "service.sh");
const relayClientScript = process.env.CONTEXT_RELAY_CLIENT_SCRIPT || join(root, "scripts", "relay-client.sh");
const passiveCaptureServiceScript = process.env.CONTEXT_GUARDIAN_PASSIVE_CAPTURE_SERVICE_SCRIPT || join(root, "scripts", "passive-capture-service.sh");
const blindRelayServiceScript = process.env.CONTEXT_GUARDIAN_BLIND_RELAY_SERVICE_SCRIPT || join(root, "scripts", "setup-blind-relay.sh");
const MAX_CHILD_OUTPUT_BYTES = 1024 * 1024;

const tools = [
  {
    name: "inspect_context",
    description: "Read a single Codex task's context status without modifying files or databases.",
    inputSchema: {
      type: "object",
      properties: {
        thread_id: { type: "string", description: "Exact Codex thread/task UUID." },
        trigger_tokens: { type: "integer", minimum: 10000, default: 200000 }
      },
      required: ["thread_id"],
      additionalProperties: false
    }
  },
  {
    name: "recover_context",
    description: "Run one scoped recovery pass for a Codex task. This may back up and rewrite its rollout and lower stale SQLite token counters.",
    inputSchema: {
      type: "object",
      properties: {
        thread_id: { type: "string" },
        confirm: { type: "boolean", description: "Must be true because recovery mutates local state." },
        trigger_tokens: { type: "integer", minimum: 10000, default: 200000 },
        recovery_tokens: { type: "integer", minimum: 10000, default: 100000 },
        large_output_bytes: { type: "integer", minimum: 10000, default: 160000 },
        cc_switch_summary: { type: "boolean", default: false, description: "Send oversized tool outputs to a trusted OpenAI-compatible API for map-reduce summarization before pruning." },
        cc_switch_url: { type: "string", pattern: "^https?://", default: "http://127.0.0.1:15721/v1/chat/completions" },
        cc_switch_model: { type: "string", minLength: 1, default: "feature/gpt-5.6-sol" },
        cc_switch_chunk_target_tokens: { type: "integer", minimum: 8000, default: 120000 },
        image_base_url: { type: "string", pattern: "^https://" },
        image_signing_key_file: { type: "string" },
        image_cache_dir: { type: "string" },
        image_url_ttl_seconds: { type: "integer", minimum: 1, maximum: 86400, default: 900 },
        message_format_preview: { type: "boolean", default: false, description: "Preview: diagnose and safely normalize damaged message envelopes after unknown task errors." },
        message_format_live_probe: { type: "boolean", default: false, description: "Send one minimal live Codex probe in the current user environment before applying message repairs." },
        message_format_passive_capture: { type: "boolean", default: false, description: "Require correlated schema-only evidence from the passive loopback sidecar before repair." },
        message_format_probe_timeout_seconds: { type: "integer", minimum: 5, maximum: 300, default: 60 },
        message_format_probe_command: { type: "string", description: "Optional path to the current user's Codex CLI binary." },
        message_format_capture_report_dir: { type: "string" },
        message_format_capture_window_seconds: { type: "integer", minimum: 30, maximum: 3600, default: 300 }
      },
      required: ["thread_id", "confirm"],
      additionalProperties: false
    }
  },
  {
    name: "guardian_service",
    description: "Install, remove, or inspect a per-task background guardian using launchd on macOS or systemd --user on Linux.",
    inputSchema: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["install", "remove", "status"] },
        thread_id: { type: "string" },
        confirm: { type: "boolean", description: "Required for install and remove." },
        large_output_bytes: { type: "integer", minimum: 10000, default: 160000 },
        cc_switch_summary: { type: "boolean", default: false },
        cc_switch_url: { type: "string", pattern: "^https?://", default: "http://127.0.0.1:15721/v1/chat/completions" },
        cc_switch_model: { type: "string", minLength: 1, default: "feature/gpt-5.6-sol" },
        cc_switch_chunk_target_tokens: { type: "integer", minimum: 8000, default: 120000 },
        message_format_preview: { type: "boolean", default: false },
        message_format_live_probe: { type: "boolean", default: false },
        message_format_passive_capture: { type: "boolean", default: false }
      },
      required: ["action", "thread_id"],
      additionalProperties: false
    }
  },
  {
    name: "passive_capture_service",
    description: "Install, remove, or inspect the read-only macOS loopback packet-capture sidecar. It never changes Codex Provider, Base URL, config, process state, or routing.",
    inputSchema: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["install", "remove", "status"] },
        confirm: { type: "boolean", description: "Required for install and remove." },
        interface: { type: "string", pattern: "^[A-Za-z0-9_.-]{1,64}$", default: "lo0" },
        port: { type: "integer", minimum: 1, maximum: 65535, default: 15721 },
        duration_seconds: { type: "integer", minimum: 1, maximum: 900, default: 60 },
        max_pcap_bytes: { type: "integer", minimum: 65536, maximum: 268435456, default: 16777216 },
        max_reports: { type: "integer", minimum: 2, maximum: 10000, default: 100 },
        report_dir: { type: "string" }
      },
      required: ["action"],
      additionalProperties: false
    }
  },
  {
    name: "relay_client_service",
    description: "Install, remove, or inspect the optional public Relay client. First start generates a private per-user identity automatically.",
    inputSchema: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["install", "remove", "status"] },
        relay_url: { type: "string", pattern: "^https://" },
        confirm: { type: "boolean", description: "Required for install and remove." }
      },
      required: ["action"],
      additionalProperties: false
    }
  },
  {
    name: "blind_relay_service",
    description: "Install, renew, remove, or inspect the preview blind TLS image Relay. TLS terminates at the local gateway so the public Relay forwards ciphertext only.",
    inputSchema: {
      type: "object",
      properties: {
        action: { type: "string", enum: ["install", "renew", "remove", "status"] },
        relay_url: { type: "string", pattern: "^https://" },
        blind_suffix: { type: "string", pattern: "^[A-Za-z0-9.-]+$" },
        acme_email: { type: "string" },
        certificate_file: { type: "string" },
        private_key_file: { type: "string" },
        confirm: { type: "boolean", description: "Required for install, renew, and remove." }
      },
      required: ["action"],
      additionalProperties: false
    }
  }
];

function validateThreadId(value) {
  if (typeof value !== "string" || !/^[0-9a-f-]{8,80}$/i.test(value)) {
    throw new Error("thread_id must contain only hexadecimal characters and hyphens");
  }
  return value;
}

function appendCcSwitchArguments(args, input) {
  if (!input.cc_switch_summary) return;
  const endpoint = input.cc_switch_url || "http://127.0.0.1:15721/v1/chat/completions";
  let parsed;
  try {
    parsed = new URL(endpoint);
  } catch {
    throw new Error("cc_switch_url must be a valid HTTP or HTTPS URL");
  }
  if (!["http:", "https:"].includes(parsed.protocol)) {
    throw new Error("cc_switch_url must use HTTP or HTTPS");
  }
  const model = input.cc_switch_model || "feature/gpt-5.6-sol";
  if (typeof model !== "string" || model.length === 0 || model.length > 200) {
    throw new Error("cc_switch_model must contain 1 to 200 characters");
  }
  const chunkTarget = input.cc_switch_chunk_target_tokens || 120000;
  if (!Number.isInteger(chunkTarget) || chunkTarget < 8000) {
    throw new Error("cc_switch_chunk_target_tokens must be an integer of at least 8000");
  }
  args.push(
    "--enable-cc-switch-summary",
    "--cc-switch-url", endpoint,
    "--cc-switch-model", model,
    "--cc-switch-chunk-target-tokens", String(chunkTarget)
  );
}

async function ensureExecutable(path) {
  await access(path, constants.X_OK);
}

function run(command, args, extraEnv = {}) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(command, args, { env: { ...process.env, ...extraEnv }, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    let outputBytes = 0;
    let outputExceeded = false;
    const append = (target, chunk) => {
      outputBytes += chunk.length;
      if (outputBytes > MAX_CHILD_OUTPUT_BYTES) {
        outputExceeded = true;
        child.kill("SIGKILL");
        return target;
      }
      return target + chunk.toString("utf8");
    };
    child.stdout.on("data", chunk => { stdout = append(stdout, chunk); });
    child.stderr.on("data", chunk => { stderr = append(stderr, chunk); });
    child.on("error", rejectRun);
    child.on("close", code => {
      if (outputExceeded) {
        rejectRun(new Error("child output exceeded 1 MiB safety limit"));
        return;
      }
      if (code === 0) resolveRun({ stdout, stderr });
      else rejectRun(new Error(stderr.trim() || stdout.trim() || `command exited with ${code}`));
    });
  });
}

async function callTool(name, input = {}) {
  if (name === "inspect_context") {
    const threadId = validateThreadId(input.thread_id);
    await ensureExecutable(binary);
    return run(binary, ["--thread-id", threadId, "--status", "--context-trigger-tokens", String(input.trigger_tokens || 200000)]);
  }
  if (name === "recover_context") {
    const threadId = validateThreadId(input.thread_id);
    if (input.confirm !== true) throw new Error("confirm=true is required for recovery");
    await ensureExecutable(binary);
    const args = [
      "--thread-id", threadId,
      "--once",
      "--context-trigger-tokens", String(input.trigger_tokens || 200000),
      "--recovery-tokens", String(input.recovery_tokens || 100000),
      "--large-tool-output-bytes", String(input.large_output_bytes || 160000)
    ];
    appendCcSwitchArguments(args, input);
    const imageOptions = [input.image_base_url, input.image_signing_key_file, input.image_cache_dir];
    if (imageOptions.some(Boolean) && !imageOptions.every(Boolean)) {
      throw new Error("image_base_url, image_signing_key_file, and image_cache_dir must be provided together");
    }
    if (imageOptions.every(Boolean)) {
      args.push(
        "--image-base-url", input.image_base_url,
        "--image-signing-key-file", input.image_signing_key_file,
        "--image-cache-dir", input.image_cache_dir,
        "--image-url-ttl-seconds", String(input.image_url_ttl_seconds || 900)
      );
    }
    if ((input.message_format_live_probe || input.message_format_passive_capture) && !input.message_format_preview) {
      throw new Error("message format live probe and passive capture require message_format_preview");
    }
    if (input.message_format_preview) args.push("--enable-message-format-preview");
    if (input.message_format_live_probe) {
      args.push(
        "--enable-message-format-live-probe",
        "--message-format-probe-timeout-seconds", String(input.message_format_probe_timeout_seconds || 60)
      );
      if (input.message_format_probe_command) {
        args.push("--message-format-probe-command", input.message_format_probe_command);
      }
    }
    if (input.message_format_passive_capture) {
      args.push(
        "--enable-message-format-passive-capture",
        "--message-format-capture-window-seconds", String(input.message_format_capture_window_seconds || 300)
      );
      if (input.message_format_capture_report_dir) {
        args.push("--message-format-capture-report-dir", input.message_format_capture_report_dir);
      }
    }
    return run(binary, args);
  }
  if (name === "guardian_service") {
    const threadId = validateThreadId(input.thread_id);
    if (!["install", "remove", "status"].includes(input.action)) throw new Error("invalid action");
    if (input.action !== "status" && input.confirm !== true) throw new Error("confirm=true is required for service changes");
    if ((input.message_format_live_probe || input.message_format_passive_capture) && !input.message_format_preview) {
      throw new Error("message format live probe and passive capture require message_format_preview");
    }
    const serviceEnv = {
      CONTEXT_GUARDIAN_LARGE_TOOL_OUTPUT_BYTES: String(input.large_output_bytes || 160000)
    };
    if (input.cc_switch_summary) {
      const ccArgs = [];
      appendCcSwitchArguments(ccArgs, input);
      serviceEnv.CONTEXT_GUARDIAN_CC_SWITCH_SUMMARY = "1";
      serviceEnv.CONTEXT_GUARDIAN_CC_SWITCH_URL = input.cc_switch_url || "http://127.0.0.1:15721/v1/chat/completions";
      serviceEnv.CONTEXT_GUARDIAN_CC_SWITCH_MODEL = input.cc_switch_model || "feature/gpt-5.6-sol";
      serviceEnv.CONTEXT_GUARDIAN_CC_SWITCH_CHUNK_TARGET_TOKENS = String(input.cc_switch_chunk_target_tokens || 120000);
    }
    if (input.message_format_preview) serviceEnv.CONTEXT_GUARDIAN_MESSAGE_FORMAT_PREVIEW = "1";
    if (input.message_format_live_probe) serviceEnv.CONTEXT_GUARDIAN_MESSAGE_FORMAT_LIVE_PROBE = "1";
    if (input.message_format_passive_capture) serviceEnv.CONTEXT_GUARDIAN_MESSAGE_FORMAT_PASSIVE_CAPTURE = "1";
    return run("sh", [serviceScript, input.action, threadId, binary], serviceEnv);
  }
  if (name === "passive_capture_service") {
    if (!["install", "remove", "status"].includes(input.action)) throw new Error("invalid action");
    if (input.action !== "status" && input.confirm !== true) throw new Error("confirm=true is required for service changes");
    const captureEnv = {
      CONTEXT_GUARDIAN_PASSIVE_CAPTURE_BIN: passiveCaptureBinary,
      CONTEXT_GUARDIAN_PASSIVE_CAPTURE_INTERFACE: input.interface || "lo0",
      CONTEXT_GUARDIAN_PASSIVE_CAPTURE_PORT: String(input.port || 15721),
      CONTEXT_GUARDIAN_PASSIVE_CAPTURE_SECONDS: String(input.duration_seconds || 60),
      CONTEXT_GUARDIAN_PASSIVE_CAPTURE_MAX_BYTES: String(input.max_pcap_bytes || 16777216),
      CONTEXT_GUARDIAN_PASSIVE_CAPTURE_MAX_REPORTS: String(input.max_reports || 100)
    };
    if (input.report_dir) captureEnv.CONTEXT_GUARDIAN_PASSIVE_CAPTURE_REPORT_DIR = input.report_dir;
    return run("sh", [passiveCaptureServiceScript, input.action], captureEnv);
  }
  if (name === "relay_client_service") {
    if (!["install", "remove", "status"].includes(input.action)) throw new Error("invalid action");
    if (input.action !== "status" && input.confirm !== true) throw new Error("confirm=true is required for service changes");
    if (input.action === "install" && (typeof input.relay_url !== "string" || !input.relay_url.startsWith("https://"))) {
      throw new Error("relay_url using https:// is required for install");
    }
    return run("sh", [relayClientScript, input.action, input.relay_url || ""]);
  }
  if (name === "blind_relay_service") {
    if (!["install", "renew", "remove", "status"].includes(input.action)) throw new Error("invalid action");
    if (!["status"].includes(input.action) && input.confirm !== true) throw new Error("confirm=true is required for blind Relay changes");
    if (["install", "renew"].includes(input.action)) {
      if (typeof input.relay_url !== "string" || !input.relay_url.startsWith("https://")) throw new Error("relay_url using HTTPS is required");
      if (typeof input.blind_suffix !== "string" || !/^[A-Za-z0-9.-]+$/.test(input.blind_suffix)) throw new Error("valid blind_suffix is required");
      const customCertificate = [input.certificate_file, input.private_key_file];
      if (customCertificate.some(Boolean) && !customCertificate.every(Boolean)) throw new Error("certificate_file and private_key_file must be provided together");
      if (!customCertificate.every(Boolean) && !input.acme_email) throw new Error("acme_email is required when no certificate pair is supplied");
    }
    const blindEnv = {};
    if (input.certificate_file) blindEnv.CONTEXT_RELAY_BLIND_CERT_FILE = input.certificate_file;
    if (input.private_key_file) blindEnv.CONTEXT_RELAY_BLIND_KEY_FILE = input.private_key_file;
    return run("sh", [
      blindRelayServiceScript,
      input.action,
      input.relay_url || "",
      input.blind_suffix || "",
      input.acme_email || ""
    ], blindEnv);
  }
  throw new Error(`unknown tool: ${name}`);
}

function respond(message) {
  process.stdout.write(`${JSON.stringify(message)}\n`);
}

async function handle(request) {
  if (request.method === "initialize") {
    return { protocolVersion: "2025-03-26", capabilities: { tools: {} }, serverInfo: { name: "context-guardian", version: "0.4.1" } };
  }
  if (request.method === "tools/list") return { tools };
  if (request.method === "tools/call") {
    const result = await callTool(request.params?.name, request.params?.arguments || {});
    return { content: [{ type: "text", text: `${result.stdout}${result.stderr}`.trim() }] };
  }
  if (request.method?.startsWith("notifications/")) return undefined;
  throw new Error(`method not found: ${request.method}`);
}

const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on("line", async line => {
  if (!line.trim()) return;
  let request;
  try {
    request = JSON.parse(line);
    const result = await handle(request);
    if (request.id !== undefined && result !== undefined) respond({ jsonrpc: "2.0", id: request.id, result });
  } catch (error) {
    if (request?.id !== undefined) respond({ jsonrpc: "2.0", id: request.id, error: { code: -32000, message: error.message } });
  }
});
