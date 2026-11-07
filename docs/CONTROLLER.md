# dokitier-console（独立控制台）设计

## 原则

- core 不再内置任何 Web 控制台，也不提供配置编辑入口。
- core 只做两件事：
  1. 主动连接控制台，上报状态/拓扑/链路/指标/日志；
  2. 接收控制台下发的配置，原子应用。
- 控制台是唯一配置源：配置的读取、编辑、版本管理、下发都只在控制台。
- 控制台直接复用当前 dokitier 前端（`dokitier-web/webui`，AstrNav 风格 Vue UI），
  不从原版 React 控制台重写界面；仅改造数据层为多节点 board。
- Rust 服务端以原版 EasyTier Web 控制台的 Agent/配置/数据库模型为参考。

## 进程

```
dokitier-console（唯一 Web 入口）
  ├─ Web UI / HTTP API / 用户鉴权（会话、RBAC、审计）
  ├─ 节点注册表、配置版本、历史指标库
  └─ Agent 接入（core 主动连入，两种独立传输）：
       ├─ WS    WebSocket 全双工（TCP，可用 wss）
       └─ QUIC  原生 QUIC 全双工（纯 UDP，不需要 TCP 放通）

dokitier-core（Agent）
  └─ [controller]
       url = "wss://console.example.com/api/agent/ws"
       token = "<node-token>"
```

## 鉴权与防劫持

不使用 mTLS。采用 **预登记节点 + 每节点 token + 挑战应答 + 会话加密**。

### 节点预登记（防未知节点注册）

- 控制台先创建节点记录，生成一次性展示的 `node_id` + `token`，
  数据库只存 `token_hash`（HMAC-SHA256）、启用状态、绑定信息。
- board 模式下 core 只需要控制台连接信息，网络配置由 board 下发：

```toml
instance_name = "minimal"
instance_id = "..."

[controller]
url = "ws://console.example.com/api/agent/ws"
node_id = "node-xxxx"
token = "<node-token>"

[flags]
no_tun = true
```

core 启动后以空实例连上 board，管理员在 board 里推送该节点的网络 TOML，core 原子应用。

原完整配置仍兼容。core 配置：

```toml
[controller]
url = "wss://console.example.com/api/agent/ws"
node_id = "node-xxxx"
token = "<node-token>"
```

- 注册校验顺序：
  1. `node_id` 必须已存在且未吊销；
  2. token 哈希匹配；
  3. 同一 node_id 已有活跃连接时，拒绝新连接或踢掉旧连接（可配置）；
  4. 失败计入限流与审计日志，连续失败临时封禁 IP。
- 未登记 node_id 一律拒绝，无法进入 board。

### 防劫持握手（WSS / TCP）

不依赖 TLS 也要保证“对方确实持有 token”，并且链路内容不可读/不可篡改：

1. board → core：`server_nonce`（32 字节随机）
2. core → board：
   - `client_nonce`、`ts`
   - `client_proof = HMAC(token, "client" | node_id | server_nonce | client_nonce | ts)`
3. board 校验 client_proof 与 node_id/token，回：
   - `server_proof = HMAC(token, "server" | node_id | server_nonce | client_nonce | ts)`
4. 双方派生会话密钥：
   `key = HKDF-SHA256(token, server_nonce || client_nonce || node_id)`
5. 之后所有帧用 AES-256-GCM 加密，帧头带单调递增序号；
   序号重复/回退直接断开（防重放）。

效果：

- 不知道 token 的节点无法完成握手，不能注册；
- 不知道 token 的中间人无法伪造 server_proof，core 不会向冒充 board 的对端发送配置；
- 握手后内容加密并带序号，无法窃听、篡改或重放；
- WSS 仍然走 TLS（验证系统根证书，可选证书指纹固定），token 握手作为第二层；
- UDP 只发心跳：`HMAC(token, node_id | ts | seq)`，60 秒窗口 + 单调 seq；
  配置/命令永远不走 UDP。

### 控制台用户鉴权

- 登录会话（HttpOnly Cookie），argon2 密码哈希；
- 角色：viewer / operator / admin；
- 配置下发、重启、命令执行全部写审计日志。

## 数据库（多后端）

- 使用 `sqlx::AnyPool`，`DATABASE_URL` 决定后端：
  - `sqlite:///var/lib/dokitier/console.db`
  - `postgres://user:pass@host/db`
  - `mysql://user:pass@host/db`
- 迁移按方言拆分（`migrations/sqlite`, `migrations/postgres`, `migrations/mysql`）；
- 表：nodes、node_configs（版本/hash）、metrics（按时间分区/索引）、audit_logs、users、sessions；
- 历史指标只存控制台，core 只保留最近 60 秒内存环形缓冲。

## 协议（控制台 ↔ core）

core → console：

- `hello`：instance_id、peer_id、hostname、version、network_name、config_hash
- `heartbeat`：5s
- `snapshot`：拓扑、节点、链路、实时指标（1–2s）
- `logs`：可选（级别、模块、最近 N 行）
- `result`：request_id + 成功/错误

console → core：

- `config.apply`：完整 TOML + version/hash；core 校验、原子写盘、原子重启实例
- `config.request`：请求当前配置（仅在首次注册/漂移校验时）
- `instance.restart`
- `logs.tail`

core 不接受任何本地配置编辑请求；控制台是唯一入口。

## 原子重启

`config.apply` 流程：

1. 校验 TOML、比较 version/hash，防回滚；
2. 写临时文件 + rename；
3. `manager.delete_network_instances([id])`；
4. `manager.run_network_instance(new_cfg)`；
5. 回传新 peer_id、listeners、状态。

## 改造路线

1. 以原版 EasyTier Web 控制台为基线建立 `dokitier-console` crate（保留 React UI，替换品牌/API）。
2. 控制台实现多后端数据库层、用户鉴权、节点表、配置版本表。
3. core 移除 `dokitier-web` 嵌入逻辑，只保留 `[controller]` Agent 客户端（WSS/TCP/UDP + 重连）。
4. Agent 上报快照；控制台展示多节点拓扑/指标，替代现有单机 UI 数据源。
5. 控制台实现配置编辑、版本下发、原子重启、审计。

## 前端复用

- 现有 `dokitier-web/webui` 原样迁到 `dokitier-console` 静态资源。
- 页面保留：仪表盘、拓扑、节点、链路、路由、历史指标、设置、登录。
- 数据源从“本机 core”改为“board 内已注册节点”：
  - 顶部/侧栏增加节点选择器（全部 / 单节点）
  - 拓扑默认展示全部节点，可聚焦单节点
  - 节点详情与配置编辑只对 board 中已登记节点开放
- 配置编辑、下发、原子重启仍由控制台后端执行，前端只调用控制台 API。
- core 不再打包或提供任何前端资源。
