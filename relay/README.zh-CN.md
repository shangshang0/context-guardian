# Context Relay Server

[English](README.md) | [简体中文](README.zh-CN.md)

在自己的公网服务器上部署与 Context Guardian 默认公共服务相同的多租户 Relay。兼容 v1 在 Relay 终止图片 HTTPS；预览版 v2 把内层 TLS 原样转发到用户本机 Gateway。

## 环境要求

- 一台安装 Docker Engine 和 Compose v2 的 Linux 服务器。
- 一个 A/AAAA记录指向该服务器的公网域名。
- 开放 TCP `80` 供 Caddy ACME 使用，并开放 `5003` 和/或 `5004` 作为控制 HTTPS 入口。
- v2 还需开放 TCP `443`，并让 `*.<blind_suffix>` 的通配 DNS 指向该服务器。

## 部署

```sh
cp .env.example .env
# 填写控制域名、ACME 联系邮箱和 blind DNS suffix。
docker compose up -d --build
curl -fsS -o /dev/null -w '%{http_code}\n' https://relay.example.com:5003/healthz
```

健康检查预期返回 `204`。Caddy 只负责自动申请和续签控制入口的证书。Relay 容器以非 root、只读、无 capabilities、资源受限、无宿主挂载方式运行。HTTPS 容器仅保留绑定容器内部 80/443 所需的 `NET_BIND_SERVICE`。图片字节和租户注册默认只存在内存中。

两种传输可以同时运行：

| 模式 | 公网数据端口 | Relay 可见内容 | 本机 Gateway |
| --- | --- | --- | --- |
| 兼容 v1 | `5003`/`5004` | 签名 URL、请求头、瞬时图片字节、元数据 | HTTP `[::1]:8787` |
| 预览 v2 | `443` | SNI、IP/时序、密文大小 | TLS `127.0.0.1:8788` |

v2 配置为：

```dotenv
CONTEXT_RELAY_BLIND_LISTEN=0.0.0.0:8443
CONTEXT_RELAY_BLIND_SUFFIX=relay.example.com
```

Compose 会把宿主机 `443` 直接映射到容器内非特权的 `8443` Relay 监听器。Server 只接受配置后缀下严格的 32 位十六进制租户标签，只读取有大小限制的 ClientHello 来选择租户，不会终止内层 TLS。不要在公网 `443` 前放 HTTP 反向代理；该端口必须保留原始 TCP/TLS。WSS 控制隧道仍通过 Caddy 的 `5003` 或 `5004`，路径为 `/v2/tunnel/<tenant_id>`。

安装客户端时指定自建服务：

```sh
CONTEXT_RELAY_URL=https://relay.example.com:5003 ./scripts/install.sh
```

如果只安装二进制、不启用任何 Relay Client：

```sh
CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY=1 ./scripts/install.sh
```

## 配置 v2 Client

租户主机名为 `<tenant_id>.<blind_suffix>`。本机脚本会先校验主机名、证书有效期、证书/私钥匹配关系，以及私钥是否为 `0400`/`0600`，然后才启动服务。

使用已有证书和私钥：

```sh
CONTEXT_RELAY_BLIND_CERT_FILE=/absolute/path/fullchain.pem \
CONTEXT_RELAY_BLIND_KEY_FILE=/absolute/path/private-key.pem \
./scripts/setup-blind-relay.sh install \
  https://relay.example.com:5003 relay.example.com
```

或者在本机安装 `acme.sh`，通过 TLS-ALPN-01 申请精确租户证书：

```sh
./scripts/setup-blind-relay.sh install \
  https://relay.example.com:5003 relay.example.com admin@example.com
```

签发时，脚本临时把盲隧道指向 `acme.sh --alpn --tlsport 8789`；常驻服务则指向本机 `8788` TLS Gateway。证书私钥不会经过 Relay。由于续签时必须运行临时隧道，续签采用显式命令：

```sh
./scripts/setup-blind-relay.sh renew \
  https://relay.example.com:5003 relay.example.com admin@example.com
```

`status` 会检查两个 launchd 服务。`remove` 会停止 v2 并删除服务/发布配置，但保留证书、身份、签名密钥与图片缓存。如需同时恢复 v1，提供控制 URL：`./scripts/setup-blind-relay.sh remove https://relay.example.com:5003`。

## 安全边界

- Client 在本机生成独立密钥；Server 在 TLS 注册阶段接收密钥，但只在内存中保存 SHA-256摘要。
- 租户 ID由密钥派生，注册还需要轻量工作量证明。
- 跨租户凭据和路径扫描统一返回 `404`。
- v1 不保存图片，但运营者能看到转发过程中的瞬时图片字节和流量元数据。
- v2 下 Relay 能看到 SNI、对端地址、时序和密文大小，但看不到 URL、HMAC 签名、HTTP 请求头或图片明文。
- 共享运营者域名签发的证书只能防御被动和“诚实但好奇”的运营方式，不能防御恶意域名所有者：所有者仍可另行签发有效证书，对未来连接主动 MITM。
- 最强边界是使用客户自有 DNS suffix/证书部署独享 Relay；Server 配置的 suffix 必须匹配租户证书主机名。
- 不要公开内部端口 `8080`；它只能在 Docker 网络内由 Caddy访问。
- 容器内 v2 `8443` 只应通过明确的宿主机 `443` 映射访问。

如需持久化租户摘要，可设置 `CONTEXT_RELAY_TENANT_STORE`，并挂载一个由 UID/GID `65532`拥有的私有可写目录。该功能不是必需的，因为 Relay 重启后客户端会自动重新注册。
