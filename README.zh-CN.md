# Context Guardian

[English](README.md) | [简体中文](README.zh-CN.md)

Context Guardian 是一个面向 Codex 任务上下文的 Rust 旁路守护工具，用于检查、修复并持续保护异常膨胀或损坏的任务上下文。它还提供可选的签名图片旁路：把大体积 Base64 图片从 rollout 历史中移除，同时通过短时 HTTPS URL 保留 GPT 多模态识图能力。

## 它解决什么问题

- 修复反复触发上下文窗口错误、无法继续运行的任务。
- 只调整指定任务的陈旧 token 计数，不修改全局 Codex 配置。
- 清理 rollout JSONL 中超大的 Base64 图片、历史附件和工具输出。
- 保留已有压缩摘要以及当前活跃对话尾部。
- 可选地把清理后的图片替换为短时签名 HTTPS URL。
- 默认可使用项目公共 Relay，也可在自己信任的服务器上通过 Docker 完整自建。

Context Guardian 每次只处理一个明确的任务/线程 ID。重要重写前会创建备份；遇到未知路径或 Codex 数据结构时会拒绝继续，而不是猜测修改。

## 架构

```text
Codex rollout/state
        │
        ▼
context-guardian ── 修复指定任务状态
        │ 可选图片签名发布
        ▼
本机 Rust Gateway（[::1]:8787）
        │ 主动出站 HTTPS 轮询
        ▼
公共 Relay 或自建 Relay
        │ 短时签名 URL
        ▼
GPT 图片抓取服务
```

图片保存在用户本机缓存中，Relay 不持久化图片。但当前第一版协议下，Relay 运营者能够看到转发过程中的瞬时图片字节和流量元数据；敏感图片建议使用自建 Relay。

## 环境要求

- 当前稳定版 Rust 工具链。
- “安装即用”的公共 Relay 后台服务目前支持 macOS。
- Guardian CLI 和 Guardian 后台服务支持 macOS/Linux。
- 只有使用 MCP 时才需要 Node.js 18+。
- Codex 状态位于 `$CODEX_HOME` 或 `${HOME}/.codex`。

Rust 二进制已内置 SQLite，不需要额外安装 `sqlite3`。

## 快速安装

```sh
git clone https://github.com/shangshang0/context-guardian.git
cd context-guardian
./scripts/install.sh
```

在 macOS 上，安装脚本会自动：

1. 构建并安装 Guardian、本机图片 Gateway、Relay Client、MCP 和服务脚本。
2. 为当前用户生成独立的 256-bit 租户密钥和派生出的 128-bit 租户 ID。
3. 以 `0600` 权限保存身份和图片签名材料。
4. 启动只监听回环地址的图片 Gateway 与公共 Relay Client。
5. 把当前用户的图片发布参数写入 `$CODEX_HOME/context-guardian/image-publishing.env`。

网络图片发布仍然按守护任务显式开启。若只安装二进制、不启用公共 Relay：

```sh
CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY=1 ./scripts/install.sh
```

只生成并校验 launchd 配置，不实际启动服务：

```sh
CONTEXT_GUARDIAN_DRY_RUN=1 ./scripts/install.sh
```

## Guardian CLI

只读检查：

```sh
context-guardian --thread-id 019f... --status
```

执行一次范围受限的修复：

```sh
context-guardian --thread-id 019f... --once
```

前台持续守护：

```sh
context-guardian --thread-id 019f...
```

rollout 路径默认从 `state_5.sqlite` 自动发现。只有自定义 Codex 布局时才需要覆盖 `--rollout`、`--state-db` 或 `--goals-db`。

## 后台守护服务

```sh
./scripts/service.sh install 019f... ./target/release/context-guardian
./scripts/service.sh status 019f... ./target/release/context-guardian
./scripts/service.sh remove 019f... ./target/release/context-guardian
```

macOS 下，`service.sh install` 会自动读取当前用户权限为 `0600` 的图片发布配置，并把四个图片参数注入守护进程；不会读取其他用户的 HOME、身份或密钥。

## 图片发布模式

### 默认公共 Relay

macOS 默认安装会连接项目维护的 HTTPS Relay。用户不需要 SSH 账号、家庭网络入站端口或手工创建客户端密钥。

每个客户端都会：

- 在本机生成独立密钥；
- 从密钥派生租户 ID；
- 计算轻量注册工作量证明；
- 独立认证轮询与返回请求；
- 对错误凭据、错误租户和目录扫描统一返回 `404`。

签名图片 URL 默认 900 秒过期。Guardian 会把已发布图片保存为协议合法的 `input_text` 引用，而不是远程 `input_image`：当前 Codex CLI 在重建历史上下文时不接受远程图片 URL。直接 API 客户端或 Agent 可在过期前显式获取该签名 URL。发布失败时，Guardian 会回退为纯文本占位符。

### 自建 Docker Relay

Relay Server 已完整开源，可部署到自己信任且具有公网域名的服务器：

```sh
cd relay
cp .env.example .env
# 填写 CONTEXT_RELAY_DOMAIN 和 CONTEXT_RELAY_ACME_EMAIL。
docker compose up -d --build
```

Caddy 会通过 80/443 自动申请并续签 HTTPS 证书。客户端安装时切换域名：

```sh
CONTEXT_RELAY_URL=https://relay.example.com ./scripts/install.sh
```

详细部署与安全边界见 [relay/README.md](relay/README.md)。

### SSH 别名备用方案

单用户或自建场景也可以使用受限 SSH 反向隧道：

```sh
./scripts/image-tunnel.sh install image-relay 5003 28787
```

`image-relay` 必须是 `~/.ssh/config` 中的普通别名。脚本拒绝原始用户名、主机名、IP和密码。服务端公钥建议限制为 `no-agent-forwarding,no-X11-forwarding,no-pty,permitlisten="0.0.0.0:5003"`。

## MCP

stdio MCP启动命令：

```sh
node /absolute/path/to/context-guardian/mcp/server.mjs
```

工具列表：

- `inspect_context`：只读检查指定任务。
- `recover_context`：执行一次修复，必须传入 `confirm=true`。
- `guardian_service`：安装/删除/检查任务守护服务，变更操作需要确认。
- `relay_client_service`：安装/删除/检查可选 Relay Client，变更操作需要确认。

MCP 会校验任务 ID和图片参数；子进程输出超过 1 MiB时会被终止，避免异常输出耗尽内存。

## Agent Skill

Skill 位于 `skill/context-guardian`，指导 Agent 完成范围受限的检查、修复、持续守护和安全图片发布。校验命令：

```sh
python3 /path/to/skill-creator/scripts/quick_validate.py skill/context-guardian
```

## 安全模型

- 严格单任务范围，rollout 路径必须包含明确任务 ID。
- rollout 或数据库重要重写前自动备份。
- Rust 图片 Gateway 只监听本机回环地址。
- 内容寻址图片文件名与 HMAC-SHA256过期签名。
- 每个用户拥有独立身份，身份文件权限为 `0600`。
- 密钥派生租户 ID、注册工作量证明、常量时间认证比较。
- 请求体、队列、并发、内存、CPU、进程数和日志均有限制。
- 错误租户、凭据、签名和扫描统一返回 `404`。
- Relay 容器非 root、只读、无 capabilities、无宿主机挂载。
- HTTPS 容器仅保留绑定 80/443所需的 `NET_BIND_SERVICE`。

运行公共 Relay 前请阅读 [SECURITY.md](SECURITY.md)。安全漏洞请通过 GitHub Security Advisories 私下报告。

## 代理支持

Relay Client 支持标准代理环境变量，包括 SOCKS：

```sh
HTTP_PROXY=http://127.0.0.1:8080 \
HTTPS_PROXY=http://127.0.0.1:8080 \
ALL_PROXY=socks5h://127.0.0.1:1080 \
./scripts/install.sh
```

本机 Gateway 请求始终绕过代理变量。

## 已知限制

- 已经在低质量压缩摘要中丢失的细节无法重新构造。
- 活跃 Codex app-server 可能短暂写回旧计数；守护模式会再次收敛。
- Codex 本地数据结构未来可能变化；未知布局会拒绝修改。
- 公共 Relay 不持久化图片，但运营者能看到瞬时图片字节和流量元数据。
- Relay Client 的后台服务安装目前仅支持 macOS；Rust Client 本身可跨平台运行。

## 开发与发布检查

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo audit --file Cargo.lock

cargo fmt --check --manifest-path relay/Cargo.toml
cargo clippy --manifest-path relay/Cargo.toml --all-targets --all-features -- -D warnings
cargo test --manifest-path relay/Cargo.toml
cargo audit --file relay/Cargo.lock

shellcheck -x scripts/*.sh skill/context-guardian/scripts/*.sh
node --check mcp/server.mjs
docker compose -f relay/compose.yaml config
```

## 许可证

MIT
