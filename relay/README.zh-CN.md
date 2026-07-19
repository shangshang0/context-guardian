# Context Relay Server

[English](README.md) | [简体中文](README.zh-CN.md)

在自己的公网服务器上部署与 Context Guardian 默认公共服务相同的多租户 Relay。

## 环境要求

- 一台安装 Docker Engine 和 Compose v2 的 Linux 服务器。
- 一个 A/AAAA记录指向该服务器的公网域名。
- 开放 TCP `80` 和 `443`，用于 ACME 和 HTTPS。

## 部署

```sh
cp .env.example .env
# 只需要修改域名和 ACME 联系邮箱。
docker compose up -d --build
curl -fsS -o /dev/null -w '%{http_code}\n' https://relay.example.com/healthz
```

健康检查预期返回 `204`。Caddy 会自动申请和续签证书。Relay 容器以非 root、只读、无 capabilities、资源受限、无宿主挂载方式运行。HTTPS 容器仅保留绑定 80/443所需的 `NET_BIND_SERVICE`。图片字节和租户注册默认只存在内存中。

安装客户端时指定自建服务：

```sh
CONTEXT_RELAY_URL=https://relay.example.com ./scripts/install.sh
```

如果只安装二进制、不启用任何 Relay Client：

```sh
CONTEXT_GUARDIAN_SKIP_PUBLIC_RELAY=1 ./scripts/install.sh
```

## 安全边界

- Client 在本机生成独立密钥；Server 在 TLS 注册阶段接收密钥，但只在内存中保存 SHA-256摘要。
- 租户 ID由密钥派生，注册还需要轻量工作量证明。
- 跨租户凭据和路径扫描统一返回 `404`。
- Relay 不保存图片，但运营者能看到转发过程中的瞬时图片字节和流量元数据。
- 不要公开内部端口 `8080`；它只能在 Docker 网络内由 Caddy访问。

如需持久化租户摘要，可设置 `CONTEXT_RELAY_TENANT_STORE`，并挂载一个由 UID/GID `65532`拥有的私有可写目录。该功能不是必需的，因为 Relay 重启后客户端会自动重新注册。
