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
const serviceScript = process.env.CONTEXT_GUARDIAN_SERVICE_SCRIPT || join(root, "scripts", "service.sh");
const relayClientScript = process.env.CONTEXT_RELAY_CLIENT_SCRIPT || join(root, "scripts", "relay-client.sh");
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
        image_base_url: { type: "string", pattern: "^https://" },
        image_signing_key_file: { type: "string" },
        image_cache_dir: { type: "string" },
        image_url_ttl_seconds: { type: "integer", minimum: 1, maximum: 86400, default: 900 }
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
        confirm: { type: "boolean", description: "Required for install and remove." }
      },
      required: ["action", "thread_id"],
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
  }
];

function validateThreadId(value) {
  if (typeof value !== "string" || !/^[0-9a-f-]{8,80}$/i.test(value)) {
    throw new Error("thread_id must contain only hexadecimal characters and hyphens");
  }
  return value;
}

async function ensureExecutable(path) {
  await access(path, constants.X_OK);
}

function run(command, args) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn(command, args, { env: process.env, stdio: ["ignore", "pipe", "pipe"] });
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
    return run(binary, args);
  }
  if (name === "guardian_service") {
    const threadId = validateThreadId(input.thread_id);
    if (!["install", "remove", "status"].includes(input.action)) throw new Error("invalid action");
    if (input.action !== "status" && input.confirm !== true) throw new Error("confirm=true is required for service changes");
    return run("sh", [serviceScript, input.action, threadId, binary]);
  }
  if (name === "relay_client_service") {
    if (!["install", "remove", "status"].includes(input.action)) throw new Error("invalid action");
    if (input.action !== "status" && input.confirm !== true) throw new Error("confirm=true is required for service changes");
    if (input.action === "install" && (typeof input.relay_url !== "string" || !input.relay_url.startsWith("https://"))) {
      throw new Error("relay_url using https:// is required for install");
    }
    return run("sh", [relayClientScript, input.action, input.relay_url || ""]);
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
