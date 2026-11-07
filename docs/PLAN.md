# dokitier 方案设计

## 1. 目标

在 EasyTier 基础上 fork 并更名为 **dokitier**，实现：

1. 每个节点内置 Web 控制台（默认关闭，配置开启）
2. Web 展示全链路拓扑：任意两节点间的协议、速率、流量、丢包
3. Web 展示所有节点详细信息
4. 支持手动静态路由
5. 支持按链路设置带宽
6. 支持全局 延迟/丢包/带宽 权重
7. 支持单链路 延迟/丢包/带宽 权重
8. 配置持久化到 SQLite
9. 配置在全网所有节点间同步
10. Web 开关、监听地址:端口、登录密码写在配置文件中，不配置默认不开启

## 2. 命名与目录改造

| 项目 | 旧 | 新 |
|---|---|---|
| 产品名 | EasyTier | dokitier |
| 二进制 | easytier-core / easytier-cli / easytier-web | dokitier-core / dokitier-cli / dokitier-web |
| Rust workspace | easytier, easytier-core, easytier-proto, easytier-web | dokitier, dokitier-core, dokitier-proto, dokitier-web |
| 安装目录 | /opt/easytier | /opt/dokitier |
| 配置目录 | /opt/easytier/config | /opt/dokitier/config |
| systemd | easytier@.service | dokitier@.service |
| 默认管理口 | 15888 | 15888（保持） |
| 环境变量 | ET_* | DOKITIER_* |
| 前端品牌 | EasyTier | dokitier |

改造步骤（一次性脚本完成，保证可编译）：

1. `git mv` crate 目录，替换 `Cargo.toml` 的 `name`、依赖路径、`[lib] name`
2. 全局替换 Rust 模块路径 `easytier_core::` → `dokitier_core::` 等
3. 替换日志 target、CLI 名称、`--version` 输出
4. proto `package easytier` → `package dokitier`，同时保留旧 package 的兼容反序列化别名（见风险）
5. 迁移脚本：`/opt/easytier` → `/opt/dokitier` 软链或一次性移动
6. 保留 `easytier` 命令为兼容入口（symlink 到 dokitier），避免运维脚本立刻失效

## 3. 总体架构

```
┌──────────────────────────── 节点进程 dokitier-core ────────────────────────────┐
│                                                                                │
│  ┌──────────┐   ┌────────────┐   ┌─────────────┐   ┌──────────────────────┐    │
│  │ 路由引擎  │◄──│ 链路注册表   │◄──│ 配置存储层    │◄──│ 配置同步 RPC           │    │
│  │ OSPF+静态 │   │ LinkRegistry│   │ SQLite      │   │ ConfigSync (overlay) │    │
│  └────┬─────┘   └─────▲──────┘   └──────▲──────┘   └──────────▲───────────┘    │
│       │               │                 │                     │                │
│  ┌────▼─────┐   ┌─────┴──────┐   ┌──────┴──────┐              │                │
│  │ PeerMgr  │──►│ 指标采集    │   │ Web 后端     │──────────────┘                │
│  │ 收发/转发 │   │ latency/loss│   │ axum+embed  │                               │
│  └──────────┘   │ rx/tx/bw   │   └──────┬──────┘                               │
│                 └────────────┘          │                                      │
└─────────────────────────────────────────┼──────────────────────────────────────┘
                                          │ HTTP
                                   ┌──────▼──────┐
                                   │ 内置 SPA 前端 │  拓扑/节点/链路/路由/设置
                                   └─────────────┘
```

关键决策：

- **Web 后端与 core 同进程**：本地节点数据直接读内存结构，不走 RPC
- **拓扑结构**：OSPF 快照 `SyncedRouteInfo` 里已有全网 `RoutePeerInfo`（节点）和 `ConnInfo.connected_peer_ids`（直连边），任意节点都能渲染完整拓扑
- **拓扑指标**：OSPF 快照**不含**非直连边的延迟/丢包/带宽/流量，这部分由端点采集后单独同步（见 8.4）
- **配置同步**：自定义 overlay RPC（复用 peer_rpc），LWW + HLC，最终一致
- **SQLite 每节点一份**：本地持久化，同步层负责收敛

## 4. 配置文件

```toml
[web_console]
enabled = false                 # 不配置该段/为 false 就完全不启动
listen = "0.0.0.0:11211"        # 默认只监听 127.0.0.1 更安全
password = ""                   # 空表示禁止登录（仍可只读？建议直接拒绝）
# 可选
tls_cert = ""
tls_key = ""
session_ttl_hours = 12

[routing]
# 全局权重，未单独配置的链路使用该值
latency_weight = 1.0
loss_weight = 20.0
bandwidth_weight = 0.5
# 参考带宽，用于带宽项归一化
reference_bandwidth_mbps = 100.0
# 成本公式：latency_ms*w_lat + loss_pct*w_loss + (ref_bw/bw)*w_bw
# 静态路由优先级高于 OSPF
static_route_priority = true
```

链路级配置与静态路由不写在节点本地 toml 里，而是写进 SQLite 并通过同步层下发（见第 6、7 节）。本地 toml 只放“本节点自己的东西”（监听、密码、调试开关），避免全网文件不一致。

## 5. SQLite 数据模型

```sql
PRAGMA journal_mode=WAL;

-- 全网配置记录的同步元数据
CREATE TABLE IF NOT EXISTS sync_meta (
    key        TEXT PRIMARY KEY,      -- 命名空间+主键，如 "link:peerA|peerB"
    namespace  TEXT NOT NULL,         -- link / static_route / settings / node_meta
    value      TEXT NOT NULL,         -- JSON
    hlc        TEXT NOT NULL,         -- 混合逻辑时钟 "物理ms:逻辑:节点uuid"
    origin     TEXT NOT NULL,         -- 写入节点 instance_id
    deleted    INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_sync_ns ON sync_meta(namespace);

CREATE TABLE IF NOT EXISTS nodes (
    peer_id     INTEGER PRIMARY KEY,
    instance_id TEXT UNIQUE,
    hostname    TEXT,
    ipv4        TEXT,
    ipv6        TEXT,
    version     TEXT,
    nat_type    TEXT,
    public_server INTEGER DEFAULT 0,
    first_seen  INTEGER,
    last_seen   INTEGER,
    meta_json   TEXT
);

CREATE TABLE IF NOT EXISTS links (
    a_peer_id   INTEGER NOT NULL,
    b_peer_id   INTEGER NOT NULL,
    bandwidth_mbps REAL,              -- NULL 表示未设置
    latency_weight REAL,              -- NULL 表示用全局
    loss_weight    REAL,
    bandwidth_weight REAL,
    disabled    INTEGER NOT NULL DEFAULT 0,
    note        TEXT,
    hlc         TEXT NOT NULL,
    PRIMARY KEY (a_peer_id, b_peer_id)
);

CREATE TABLE IF NOT EXISTS static_routes (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    dest_cidr  TEXT NOT NULL,          -- 10.0.0.0/8 / 2001:db8::/32
    next_hop_peer_id INTEGER NOT NULL,
    metric     INTEGER NOT NULL DEFAULT 100,
    enabled    INTEGER NOT NULL DEFAULT 1,
    note       TEXT,
    hlc        TEXT NOT NULL,
    UNIQUE(dest_cidr, next_hop_peer_id)
);

CREATE TABLE IF NOT EXISTS global_settings (
    key   TEXT PRIMARY KEY,            -- routing.latency_weight 等
    value TEXT NOT NULL,
    hlc   TEXT NOT NULL
);

-- 指标历史（不参与同步，本地环形保留）
CREATE TABLE IF NOT EXISTS metrics_history (
    ts          INTEGER NOT NULL,
    peer_id     INTEGER NOT NULL,
    latency_ms  REAL,
    loss_pct    REAL,
    rx_bps      REAL,
    tx_bps      REAL,
    bandwidth_mbps REAL
);
CREATE INDEX IF NOT EXISTS idx_metrics_ts ON metrics_history(ts);
```

同步层只同步 `sync_meta`（所有 namespace 的统一日志），业务表是它的物化视图；启动时从 `sync_meta` 重建 `links/static_routes/global_settings`。这样同步与业务解耦。

## 6. 配置同步协议

### 6.1 记录格式

```protobuf
message ConfigRecord {
  string key = 1;        // "link:123|456"
  string namespace = 2;
  bytes  value = 3;      // JSON
  string hlc = 4;
  string origin = 5;
  bool   deleted = 6;
}

message SyncRequest {
  string node_id = 1;
  map<string,string> key_versions = 2;  // key -> hlc（摘要）
}
message SyncResponse {
  repeated ConfigRecord missing = 1;    // 对端缺少/较旧的记录
  map<string,string> key_versions = 2;
}
```

新增 proto service：

```protobuf
service ConfigSync {
  rpc Sync(SyncRequest) returns (SyncResponse);
  rpc Push(stream ConfigRecord) returns (PushAck);
}
```

### 6.2 冲突与时钟

- 每次写入生成 HLC：`物理毫秒:逻辑计数:instance_id`
- 合并规则：**同 key 取 HLC 较大者**；相等则比较 origin 字典序，保证确定性
- 删除写墓碑（deleted=1, value 空），保留 7 天后 GC
- 节点的 `instance_id` 必须唯一（当前配置已保证；重复会导致 LWW 抖动）

### 6.3 同步流程

1. **写本地**：Web/RPC 写入 → 本地 SQLite 事务写 `sync_meta` + 业务表 → 异步广播
2. **广播**：向所有直连 peer 发 `Push`，每个 peer 再转发（洪泛，带 `record_key+hlc` 去重缓存）
3. **反熵**：每 60s 随机选 3 个 peer 调 `Sync`，交换 `key_versions` 摘要，拉取缺失记录
4. **启动**：加载 SQLite → 立即向所有 peer 发起一次全量摘要交换
5. **状态页**：Web 显示每个 key 的本地/远端版本、最后同步时间、冲突计数

### 6.4 失败与安全

- RPC 超时 3s，失败重试退避 1s/5s/30s
- 只接受同 `network_name` + `network_secret` 的 peer（复用现有握手）
- 可选：配置签名（network_secret 派生 HMAC），防止伪造配置注入
- 单条记录大小上限 64KB

## 7. 路由改造

### 7.1 成本公式

当前 `calculate_cost` 只返回延迟。改为：

```rust
fn calculate_cost(&self, src: PeerId, dst: PeerId) -> i32 {
    let link = self.link_registry.get(src, dst);   // 单链路配置
    if link.disabled { return AVOID_RELAY_COST; }

    let global = self.routing_weights;
    let w_lat  = link.latency_weight.unwrap_or(global.latency_weight);
    let w_loss = link.loss_weight.unwrap_or(global.loss_weight);
    let w_bw   = link.bandwidth_weight.unwrap_or(global.bandwidth_weight);

    let latency = self.metrics.latency_ms(src, dst);          // 已有
    let loss    = self.metrics.loss_pct(src, dst);            // loss_rate_stats 暴露
    let bw      = link.bandwidth_mbps.unwrap_or(global.reference_bandwidth_mbps); // 静态

    let cost = latency * w_lat
             + loss * w_loss
             + (global.reference_bandwidth_mbps / bw) * w_bw;

    cost.clamp(1.0, AVOID_RELAY_COST as f64 - 1.0) as i32
}
```

- 两张表（LeastHop / LeastCost）都使用新成本
- LeastHop 的“跳数归一化”保持不变，只影响等价路径的二级排序
- `latency_first = true` 时走 LeastCost

### 7.2 指标来源

| 指标 | 来源 | 现状 |
|---|---|---|
| latency | peer_conn_ping / direct_peers.latency_ms | 已有 |
| loss | peer_conn_ping.loss_rate_stats | 已有，未接入路由 |
| rx/tx 流量 | peer conn stats | 已有，展示用 |
| bandwidth | 链路配置（静态） | 新增，主用 |
| 被动吞吐 | rx/tx 时间序列估算 | 可选，仅展示 |

带宽不做在线主动探测（会干扰业务、且多路径下测量不准）；以“手工设置带宽”为准，展示层可叠加被动吞吐。

### 7.3 静态路由

发送路径上优先查静态表：

```
static_routes 按 dest 做最长前缀匹配
  命中且 enabled → 使用指定 next_hop，跳过 OSPF 下一跳
  未命中 → 走 OSPF 表
```

- 静态路由也需要 next_hop 可达（在 OSPF 表中存在），否则回退 OSPF 并告警
- 支持 metric 小的优先；相同 dest 多条时为等价多路径（ECMP）或主备，一期先做单条 + metric

### 7.4 禁止链路

链路配置 `disabled=1` → 边权设为 `AVOID_RELAY_COST`，同时不允许该 peer 成为 next_hop（软禁用）；需要硬禁用时在两端断开连接。

## 8. Web 控制台

### 8.1 启动条件

`web_console.enabled = false` 或整段缺失 → 不创建 listener、不加载前端资源、不注册路由。启动时日志明确打印：

```
web console disabled (web_console.enabled = false)
```

`enabled = true` 且 `password` 为空 → 拒绝启动并在日志报错（避免裸奔）。

### 8.2 技术栈

- 后端：`axum` + `tower-http`（静态资源、CORS 关闭）+ `rust-embed`（SPA 打包进二进制）
- 前端：Vue 3 + Vite + ECharts（拓扑图）+ Element Plus
- 认证：`POST /api/login` → argon2 校验 → 签发 HMAC session token（HttpOnly Cookie）；除 `/api/login` 外全部鉴权
- 单文件嵌入式部署，不依赖外部静态目录

### 8.3 API

```
POST /api/login                     登录
POST /api/logout
GET  /api/overview                  节点数/链路数/健康度
GET  /api/topology                  节点+边+实时指标（一次返回全图）
GET  /api/nodes                     所有节点详情
GET  /api/nodes/:peer_id            单节点详情（连接、路由、配置、日志摘要）
GET  /api/links                     所有链路（协议/rx/tx/延迟/丢包/带宽/权重）
PUT  /api/links/:a/:b               修改单链路带宽/权重/禁用
GET  /api/routes                    路由表（OSPF + 静态）
POST /api/static-routes             新增静态路由
DELETE /api/static-routes/:id
GET  /api/settings                  全局权重
PUT  /api/settings                  修改全局权重
GET  /api/sync                      同步状态（每个 key 的版本/延迟）
GET  /api/logs?peer_id=&limit=      节点日志（本地直接读，远端经 RPC）
WS   /api/events                    拓扑/指标实时推送
```

### 8.4 拓扑与指标的覆盖范围（重要）

OSPF 快照里有什么、没有什么：

| 数据 | 在 OSPF 快照中 | 说明 |
|---|---|---|
| 全部节点（含非直连） | ✅ | `RoutePeerInfo`，含 hostname/ipv4/nat/version/feature |
| 全部直连边（含非本机） | ✅ | `PeerConnInfo.connected_peer_ids` 邻接表 |
| 非直连边的延迟 | ❌ | 快照里没有逐边指标 |
| 非直连边的丢包/带宽/流量 | ❌ | 只有连接端点本地有统计 |
| 本机直连的延迟 | ✅ 局部 | 本地 `peer_conn_ping` / `direct_peers.latency_ms` |

所以“任意两个节点间连接协议、速度、流量、丢包率”不能只靠一个节点的 OSPF 快照：

- **协议/边是否存在**：快照里有（邻接表），但 tcp/quic 属于本机连接属性，非直连边只有“两端有连接”这一事实
- **延迟/丢包/rx/tx/带宽**：只有边的两个端点知道，必须由端点上报

实现方式（1 秒粒度）：

1. 每个节点**每秒**采集一次本机所有直连连接：`peer_id, tunnel_type, latency_ms, loss_pct, rx_bps, tx_bps, bandwidth_mbps`
   - `rx_bps/tx_bps` 用 1 秒窗口的字节增量计算，不用累计值
   - 延迟/丢包用最近 30 秒滑动窗口，避免单秒抖动导致路由震荡
2. **每秒**把本节点的所有直连边聚合成一条 `MetricsBatch`（不是每链路一条），通过 overlay 广播；批内每条边带 `(a_peer, b_peer, seq)` 版本
3. 广播策略：
   - 本地变化立即发；无变化时每秒发一次心跳批次（只含时间戳，减少流量）
   - 批次带 TTL=2 跳洪泛，收到即转发；用 `(origin, seq)` 去重
4. 每个节点把收到的指标写入内存 `link_metrics_latest`（覆盖式）；**SQLite 每 60 秒批量 flush 一次**（只保留用于 24h/7d 折线图）
5. Web 后端通过 **WebSocket 每秒推送**全量拓扑指标差量（只推变化的边），前端 1 秒刷新
6. 历史数据按 **60 秒**采样写入 `metrics_history`，保留 7 天；前端提供 24h（60s 桶）与 7d（600s 桶）折线图
7. 边两端上报冲突：取较新的 `(seq, origin)`；协议取两端并集（一端 tcp 一端 quic → `tcp/quic`）
8. 端点离线时指标保留最后值并标记 `stale`，60 秒后变灰

```sql
CREATE TABLE IF NOT EXISTS link_metrics_latest (
    a_peer_id INTEGER NOT NULL,
    b_peer_id INTEGER NOT NULL,
    tunnel_types TEXT,
    latency_ms REAL,
    loss_pct REAL,
    rx_bps REAL,
    tx_bps REAL,
    bandwidth_mbps REAL,
    source_peer_id INTEGER,
    hlc TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (a_peer_id, b_peer_id)
);
```

代价：50 节点 × 每节点约 10 条直连，每秒批次约 2–4 KB，洪泛到全网每节点约 0.1–0.2 MB/s；若网络紧张可把广播降频到 2–5 秒，本地采集和 Web 推送仍保持 1 秒。



### 8.5 页面

1. **拓扑**：ECharts graph，节点大小=流量，边颜色=延迟，边粗细=带宽；悬停显示 协议/延迟/丢包/rx/tx/带宽/权重；点击节点打开详情
2. **节点**：表格 + 详情抽屉（版本、NAT、instance_id、tun 地址、连接列表、路由）
3. **链路**：可编辑表格（带宽、三个权重、禁用），保存即写 SQLite 并广播
4. **路由**：OSPF 表 + 静态路由增删
5. **设置**：全局权重、参考带宽、同步状态
6. **审计/日志**：最近配置变更、来源节点、HLC

## 9. 实施里程碑

### P0 改名 + 骨架（1 周）

- crate/二进制/目录/systemd 改名，兼容 symlink
- `dokitier-web` crate 空壳，配置段解析，默认关闭
- 验收：`dokitier-core --version`、服务名、旧命令仍可用

### P1 SQLite + 本地 Web（2 周）

- 建库、schema、迁移
- 拓扑/节点/链路只读页面 + 登录
- 指标采集（延迟/丢包/rx/tx）接入 Web
- 验收：任节点打开 Web 能看到全图与实时指标

### P2 配置同步（2 周）

- proto ConfigSync service
- HLC + LWW + 墓碑 + 反熵
- 链路/静态路由/全局权重三类记录同步
- 验收：A 节点改权重，3 秒内全网生效；断网恢复后自动补齐

### P3 路由权重 + 静态路由（2 周）

- `calculate_cost` 改造，接入 loss/bandwidth/权重
- 静态路由查表优先
- 链路禁用
- 验收：改单链路权重后 `easytier-cli route` 的下一跳按预期变化

### P4 完善（1 周）

- 审计、同步状态页、指标历史、告警
- 压力测试（50 节点拓扑渲染、100 条链路配置同步）

总工期约 8 周（单人）。

## 10. 代码改动清单

| 路径 | 改动 |
|---|---|
| `Cargo.toml` | workspace 成员改名 |
| `easytier-core/src/config/mod.rs` | 新增 `web_console`、`routing` 配置段 |
| `easytier-core/src/config/toml.rs` | 序列化/反序列化、默认值、校验 |
| `easytier-core/src/peers/route/peer_ospf_route.rs` | 成本函数、静态路由优先、禁用链路 |
| `easytier-core/src/peers/peer_center/instance.rs` | `RouteCostCalculatorImpl` 接入指标/权重 |
| `easytier-core/src/peers/peer_manager.rs` | 发送路径静态路由查表 |
| 新 crate `dokitier-store` | SQLite、schema、DAO、HLC |
| 新 crate `dokitier-web` | axum、auth、API、SPA 嵌入 |
| `easytier-proto/proto/*.proto` | ConfigSync service、指标消息 |
| 新 crate `dokitier-sync` | 广播、反熵、冲突处理 |
| `easytier-core/src/instance/instance.rs` | 启动/关闭 web 与 sync 服务 |

## 11. 风险

1. **包名替换导致 proto 不兼容**：混合版本网络会握手失败。建议 fork 后统一升级；如需灰度，proto package 保持不变，仅产品名改。
2. **配置同步冲突**：两个节点同时改同一链路，LWW 可能覆盖人工修改；Web 显示冲突提示，必要时加“锁定”。
3. **权重导致路由震荡**：丢包率抖动会频繁改边权；对指标做滑动窗口（30s 均值）和最小变更阈值（cost 变化 <10% 不重算）。
4. **Web 安全**：默认关闭 + 强密码 + 只监听内网/回环；禁止空密码启动；Cookie SameSite=Strict。
5. **SQLite 并发**：WAL + 单写线程（channel 串行化），避免 `database is locked`。
6. **静态路由环路**：静态 next_hop 必须可达且非自身，保存时校验；运行时检测下一跳是否指向自己。

## 12. 后续可选项

- 配置签名（HMAC）与只读节点
- 链路带宽在线测量（packet-pair / 被动 TCP 吞吐估算）
- 多路径 ECMP 与按流哈希
- 配置仓库的“期望状态 vs 实际状态”漂移检测
- Prometheus exporter

---

## 13. 实现进度（2026-11-06）

### 已完成

- 仓库改名：crate / 二进制 / 目录 / clone，`cargo check -p dokitier` 通过
- 配置新增：
  - `[web_console]`：enabled/listen/password/session_ttl_hours/db_path，缺省关闭，空密码拒绝启动
  - `[routing]`：latency_weight / loss_weight / bandwidth_weight / reference_bandwidth_mbps
- `dokitier-web` crate：
  - axum 服务、SPA 静态资源（include_dir 嵌入）、登录/登出、Cookie 会话、argon2 密码
  - SQLite schema：sync_meta / nodes / links / static_routes / global_settings /
    link_metrics_latest / metrics_history（WAL）
  - API：overview、topology、nodes、links（读写）、routes、static-routes（增删）、
    settings（读写）、history（24h/7d）、sync（状态/导出/导入）、events（WebSocket）
  - 指标：1 秒采样写内存环形缓冲（60 个点），每 60 秒落库一次
- 指标采集：读取 PeerConn 的 latency、loss_rate、rx/tx 字节增量，按链路聚合
- 前端（Vue3 + Vuetify + ApexCharts + ECharts，astrbot 配色）：
  - 登录、仪表盘、拓扑（1s WebSocket 刷新）、节点、链路编辑、静态路由、历史折线、设置
- 端到端验证：
  - 构建产物 `target/debug/dokitier-core` 可启动
  - SPA `/` 200，未登录 `/api/overview` 401
  - 登录后 overview 返回 routing 权重
  - 链路 PUT 后 `/api/sync/export` 生成 LWW 记录
  - 静态路由新增/非法 CIDR 400 校验

### 第二轮追加

- 全量拓扑：新增 `CoreInstance::conn_snapshots()`，从 OSPF `conn_map` 导出全局邻接，
  拓扑页现在能看到非本机直连边；本机边仍带 tunnel 类型
- 加权路由：
  - `DirectConnectedPeerInfo` 增加 `loss_rate`，peer center 上报丢包
  - 新增 `dokitier-core/src/peers/link_policy.rs`：进程级链路策略、全局权重、静态路由表
  - `RouteCostCalculatorImpl` 改为
    `latency*w_lat + loss_pct*w_loss + (ref_bw/bandwidth)*w_bw`
  - 策略变更通过 generation 计数器触发下一轮路由重算，无需重启
  - 单链路 disabled → 成本 `i32::MAX-1`
  - 静态路由以最高版本注入 CIDR 前缀表，覆盖 OSPF proxy CIDR
- Web 写入链路/权重/静态路由后调用 `reload_routing_policy`，实时推入路由层
- 单元测试：加权成本、disabled、单链路覆盖、静态路由表（全部通过）

### 第三轮：自动配置同步（HTTP 拉取版）

实现方式（v1，先可用）：

- 控制台新增 `/api/sync/export`、`/api/sync/import`，使用 `x-dokitier-sync-token`
  鉴权；token = network_secret 的 digest（hex），同一网络内所有节点一致
- 每个节点每 5 秒通过 overlay 拉取所有已知节点的 `/api/sync/export`：
  `http://<peer_overlay_ip>:<web_port>/api/sync/export`
- 记录按 HLC 做 LWW 合并，写入 `sync_meta`，并物化到
  `links` / `global_settings` / `static_routes`
- 应用后调用 `reload_routing_policy`，加权路由立即生效
- 单元测试覆盖：HLC 新旧、墓碑删除、settings 与静态路由物化

限制：

- 需要参与同步的节点都启用控制台，且 `listen` 能从 overlay 访问
  （建议 `0.0.0.0:11211`，并用防火墙限制到 overlay 网段）
- 节点时钟偏差过大时 HLC 冲突判断会偏向快时钟节点

### 第四轮：拓扑指标跨节点 + 测试

- `/api/links/export`（token 鉴权）导出本机链路指标；同步 worker 每 5 秒拉取每个
  对端的链路指标到内存缓存，拓扑页可显示**非本机直连边**的协议/延迟/丢包/流量
- `/api/topology` 合并本机指标 + 远端缓存（本机优先）
- 新增测试：
  - 同步端到端：真实 HTTP 对端 → 拉取 → 物化 link/static_route
  - 指标历史分桶与 7 天清理
- 控制台日志纳入 `dokitier_web` target

### 第四轮真机联调（47 + 162 测试网）

- 独立测试网络 `dokitier-testnet`：10.222.0.x，独立 tun `doktest0/1`，控制台 11311/11312
- A（47）改链路配置 → 6 秒内 C（同机第二实例）`/api/sync` 出现相同 HLC 记录，
  日志 `applied remote config records ... applied=1`
- overlay 内三节点互通（A↔B、A↔C、B↔C）
- 发现并修复：禁用链路的成本原为 `i32::MAX-1`，低于 OSPF 的
  `AVOID_RELAY_COST`，导致“禁用”后仍按一跳直连（path_latency 巨大但不绕行）；
  改为 `i32::MAX` 后禁用边被视为不可用，路由会绕行
- 新增 `[web_console] sync_peers = ["ip:port", ...]`，可显式指定同步对端
  （跨端口/无 overlay 场景）

### 第五轮：控制台关闭语义与同步可视化

- 未开启控制台的节点：不创建 SQLite、不采样、不落库、不启动同步 worker，
  也不会被其他节点拉取指标（无 `/api/links/export`）
- `[routing]` 权重在控制台关闭时仍然应用到 OSPF 成本；开启时若 SQLite 为空，
  用 TOML 值播种 `routing.*`（避免默认值覆盖 TOML）
- `/api/sync` 增加 `summary`（记录数、命名空间分布、最近更新时间）
- 设置页改为同步记录表格 + 计数卡片，不再显示原始 JSON

### 第六轮：加权选路默认生效

- 新增 `[routing] mode = "weighted" | "least_hop"`，默认 `weighted`
- `weighted` 模式下所有包使用按 `latency/loss/bandwidth` 加权的最低价路径，
  不再受“最少跳数优先”限制（此前权重只在同跳数时参与比较）
- `least_hop` 保留旧行为：先比跳数，权重仅做等价路径比较
- 控制台设置页增加“选路模式”下拉
- 单元测试：成本公式、禁用边、静态路由
- 真机（47 A ↔ B ↔ C）验证：禁用 A-C 直连后，A→C 立即改为 A→B→C
  （cost 1→2，next_hop 变为 B），无需重启

### 第七轮：同步运行时状态

- 每次拉取记录对端结果：成功/失败、最后成功时间、错误信息、应用条数
- `/api/sync` 增加 `runtime`（last_run + peers 状态）
- 设置页展示对端同步状态表（正常/失败、最后成功时间、错误）
- 本地验证：`/api/sync` 返回 `runtime.last_run` / `runtime.peers`

### 第八轮：退避与冲突提示

- 同步失败按对端指数退避：5s → 10s → 20s → 40s … 上限 300s，成功后清零
- `SyncRuntime` 记录每对端 failures / next_retry / conflicts
- LWW 合并时检测冲突：同 key、非墓碑、值不同且远端 HLC 不更新 → `conflicts += 1`
- 设置页同步状态表新增“失败/冲突”列
- 测试：退避进入与恢复、冲突计数（9 个 web 测试全过）

### 第九轮：前端渲染修复与视觉验证

- 修复 Vuetify 组件未全局注册导致页面无样式的问题
  （`createVuetify({ components, directives })`）
- 用 Headless Chrome 截图验证登录页：蓝色主题卡片、图标、输入框、按钮均正常渲染
- 资源 hash 更新后需 `touch dokitier-web/src/api.rs` 强制重编译，
  否则 include_dir 不会重新嵌入前端文件

### 第十轮：登录后页面截图验收

- 新增 `scripts/cdp-screenshot.mjs`：通过 Chrome DevTools Protocol 登录并截图
- 已验收（截图在 `docs/screenshots/`）：
  - login.png：蓝色主题卡片、图标、输入框、按钮
  - dashboard.png：侧栏、概览卡片、路由权重表
  - settings.png：全局权重表单、选路模式、同步状态表
  - topology.png：ECharts 拓扑画布、图例、实时标识
- 注意：`vite` 增量缓存会保留旧模块，修改拓扑/图表后需
  `rm -rf dist node_modules/.vite` 再构建；Rust 侧需 `cargo clean -p dokitier-web`
  或 touch 后重编，确保 include_dir 重新嵌入
- 遗留：拓扑节点名仍渲染在节点中心（ECharts graph label position 未生效），
  属视觉细节，后续改用 `labelLayout` 或外部 HTML 标签

### 第十一轮：双实例真实数据验收

- 本机启动两个 `no_tun` 实例（A: 10.250.0.1 / C: 10.250.0.2，loopback TCP 互联）
- 验证：
  - `/api/topology` 返回 2 节点 1 边、tunnel_types=["tcp"]、1 条实时指标
  - 60 秒后 `/api/history` 出现持久化数据点（60s 桶）
  - 历史页 24h/7d 折线图渲染正常（手动注入 144 个历史点验收）
  - 链路页显示主机名、协议去重、实时 rx/tx
- 修复：
  - ApexCharts 组件未注册 → 历史图表改为直接用 ECharts 渲染
  - 拓扑节点标签用 `labelLayout: () => ({ dy: 22 })` 下移
  - CDP 截图脚本增加 cache-busting query（hash 路由不会重新加载文档，
    之前一直截到旧构建）
  - 链路页名称取自 `/api/topology`，包含本机节点

### 第十二轮：依赖清理

- 移除不再使用的 `apexcharts` / `vue3-apexcharts`（历史图已改用 ECharts）
- 重新构建并截图验证历史折线图仍正常
- README 增加截图与完整状态说明

### 第十三轮：同步 E2E 与静态路由修复

本机双实例（loopback sync_peers）验证：

- 全局权重：A 改 `routing.latency_weight=7.77` → 8s 内 C 的 `/api/settings` 生效
- 链路配置：A 改 bandwidth=42 → C `/api/links` 显示 42
- 静态路由新增：A 添加 → C `/api/routes.static` 出现
- 静态路由删除：A 删除 → C 静态路由清空（墓碑生效）

修复的两个 bug：

1. `insert_static_route` 没有写同步记录（之前静态路由根本不同步）
2. 静态路由墓碑 value 为空，materialize 时 JSON 解析失败导致远端不删除；
   改为从 key（`static_route:<cidr>|<peer>`）解析并删除
3. 新增回归测试：静态路由同步事件、墓碑删除

`cargo test -p dokitier-web` → 11 passed。

### 第十四轮：本地 Docker 交叉构建

- 构建机 `172.18.10.6` 长期不可达，改用本机 Docker（OrbStack）
- 命令：`docker run --platform linux/amd64 rust:1.95-bookworm`，
  安装 `protobuf-compiler clang libclang-dev cmake pkg-config libssl-dev zlib1g-dev mold`
- 源码与 target 挂载进容器，产物 `target/release/dokitier-core`（x86_64 Linux）
- 注意：仓库 `.cargo/config.toml` 指定 `-fuse-ld=mold`，容器内必须装 mold

### 第十五轮：Docker 产物真机验收（162）

- Docker（OrbStack，linux/amd64 + mold）构建成功：9m32s，产物 33MB x86_64
- 在 162 上跑 3 个 `no_tun` 实例（dok3-a/b/c，eth0 传输，控制台 11411-11413）
- 验证：
  - 拓扑：3 节点、3 条边、3 条实时指标
  - 加权选路：A-C 直连延迟权重设为 10000 后，
    `next_hop_latency_first` 变为 B，`cost_latency_first=2`；
    而 least-hop 表仍是直连（cost=1, path_latency=10000）→ weighted 默认模式生效
  - 配置同步：A 改 loss_weight=33 → C 8 秒内生效；静态路由新增/删除同步到 C
- 坑：Linux 上监听 127.0.0.1 时，EasyTier 拨号选源地址为 eth0 IP，
  导致 SYN 发到 eth0 而超时；测试改用 eth0 地址监听/拨号

### 第十六轮：拓扑交互与多链路

- 链路点击：点曲线打开右侧 drawer，显示该链路协议/延迟/丢包/收发，
  可直接编辑带宽、三项权重、禁用并保存（写入 SQLite 并同步）
- 多直连链路：
  - 后端采样按 `tunnel_type` 分组，计算每条隧道的延迟/丢包/rx/tx
  - `/api/topology` 的 metrics 增加 `tunnels[]`
  - 前端同一对节点之间按隧道展开多条贝塞尔曲线，标签常显 `协议 · 延迟 · 带宽`
- 用本地 3 实例同时开 tcp/quic 监听验证：每对节点出现 tcp、quic 两条曲线

### 待完成

- 47 节点当前不可达，未做第三台跨机验证
- 生产节点尚未升级到 dokitier（需用户确认后部署）
- 冲突详情查看（目前只计数）
- 全量节点同步的更大规模压测（50+ 节点）
- 历史降采样、保留策略与 7d 图表真机验证
- 在真实节点上验证加权选路与静态路由转发

### 第十七轮：P2P 分组隔离

- 新增顶层配置 `p2p_group`，握手（含 secure/noise 路径）交换分组
- 自动 P2P（Direct/TcpHolePunch/UdpHolePunch）在准入阶段校验分组，
  不同组直接拒绝
- 手工 `[[peer]]`（Manual/Listener）跨组仍可连接，用于专线/中继场景
- 旧版本对端未携带分组时保持兼容
- 单元测试覆盖同组/跨组/空分组规则
