# cloud-mint：chatgpt.com 中继 + 按需铸票（relay）

> cpr 默认用 **native** 模式自己经账号代理铸票，不需要本 relay；只有想用别的出口（如阿里云 FC）铸票时才把「打票方式」切到 relay 并部署它。

`index.js` 原本是阿里云函数计算（FC 3.0 Web 函数）的零依赖 Node 服务，也可以直接作为容器跑在 89 上。
cpr 的 turn-state 「云端打票」向它发 `X-Relay-Mint` 请求，它以账号的 `Authorization` / `Chatgpt-Account-Id`
向上游发 codex ping 铸票，验收票长（780）、目标网关（`__oailb` JWT 内嵌 `unified-N`）与上游模型声明，
返回 `{ cookies: {__cflb, __oailb}, tickets: {model: {turn_state, expires_at, ...}} }`。

## 89 上以容器运行（推荐 host 网络，只绑回环）

```bash
cd /opt/sub2api/cpr-cloud-mint   # 任意目录，放 index.js + Dockerfile
docker build -t cpr-cloud-mint:local .
docker run -d --name cpr-cloud-mint --restart unless-stopped --network host \
  -e RELAY_MODE=mint \
  -e RELAY_KEY="$(openssl rand -hex 24)" \
  -e MINT_GATEWAY=unified-95 \
  -e MINT_TICKET_LEN=780 \
  -e MINT_TICKET_TTL_S=240 \
  -e RELAY_BIND=127.0.0.1 \
  -e FC_SERVER_PORT=9000 \
  cpr-cloud-mint:local
```

`--network host` 是为了 `RELAY_BIND=127.0.0.1` 真正只在宿主回环上监听；cpr 容器如果不是 host 网络，
改成 `-p 127.0.0.1:9000:9000` 并让 cpr 通过 `host.docker.internal` 或 compose 网络访问。

出口：relay 直连上游（`agent:false`，不认代理）。放在 89 上出口就是 89 的 IP；需要走别的出口时用
`X-Edge-IP` 钉 Cloudflare 边缘，或把 relay 部署到别处并在 cpr 设置里填它的地址。

## cpr 侧配置

后台「state 观测」页 → 「云端打票」：启用、relay 地址（`http://127.0.0.1:9000`）、`X-Relay-Key`、
目标网关、票长、票有效期、模型列表（空 = 按业务请求的模型）。设置落在 `<runtime_data_dir>/turn_state/settings.json`（0600）。

## 本地联调

```bash
RELAY_MODE=mint RELAY_KEY=test RELAY_UPSTREAM=http://127.0.0.1:18090 MINT_GATEWAY=unified-95 \
  FC_SERVER_PORT=18095 RELAY_BIND=127.0.0.1 node index.js
curl -H 'X-Relay-Key: test' -H 'X-Relay-Mint: unified-95' -H 'Authorization: Bearer <AT>' \
  -H 'Chatgpt-Account-Id: <id>' -H 'X-Mint-Models: gpt-6-astra' http://127.0.0.1:18095/
```

`RELAY_UPSTREAM` 只允许 https 或本机 http（联调假上游）。
