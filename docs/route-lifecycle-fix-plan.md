# 路由生命周期修复方案：中继不通 / 僵尸节点 / p2p_group 语义

状态：已实施（v2，含路由库摘要反熵）
涉及仓库：dokitier（对照上游 EasyTier）
日期：2026-11-10

## 生产故障定位结论（2026-11-10 11:20，证据来自 126.2/126.3/126.254/127.1 四台实机）

- 127.1（HKBN）上 `easytier-cli route/peer` 中 **126.2 完全缺失**；ping 窗口内
  `traffic_packets_tx` 无增量 —— 127.1 的 core 把回给 126.2 的 ICMP reply 直接丢弃
  （无法解析目的 peer）。
- 126.2 → 127.1 的**正向转发是通的**（127.1 tun0 抓到 echo request 且内核立即回了
  reply），仅回程黑洞；126.3/126.254 的路由与转发均正常（counter-delta 与抓包证实）。
- 因果链：重启风暴 → 126.2 在 127.1 视野短暂不可达 15s → GC+墓碑删除 → 墓碑过期后
  **补拉机制失效**（collect_missing_peer_ids 对 info+conn 双缺的 peer 跳过；会话水位
  dst_saved 仍压着旧 ack 版本，发送方永不再发）→ 127.1 永久缺 126.2 → 回程黑洞。
- 结论：缺少**路由库一致性校验**机制。OSPF 用 DBD/LSR、BGP 用全表刷新解决此类问题；
  easytier 的水位 + conn 引用式 missing 探测对"非直连 peer 的纯 peer_info 行"无法自愈。

## v2 修复：路由库摘要反熵（OSPF DBD/LSR 等价物）

- proto 新增 `RouteInfoSummaryEntry { peer_id, peer_info_version, conn_info_version }`，
  同步 request/response 各携带一份全量摘要（~12B/peer，25 节点约 300B/轮）。
- 每轮同步（1s 周期）双向交换摘要；处理对方摘要时，凡"我持有的行对方缺失或版本更旧"
  → 清除该 peer 的会话 ack 水位 + 加入 pending，重发直至对端 ack（复用现有机制）。
- 收敛性：任何删条目事件（GC、墓碑、重启、异常 bug）造成的失步在 1-2 个同步周期内
  自动修复，不再依赖任何启发式。
- 与墓碑的配合：接受守卫继续拦截墓碑下限以下的重放（防复活不受影响）；TTL 过期后
  摘要差分自动触发补拉，彻底关掉"过期后永久缺失"的窗口。

---

## 一、问题根源

### 1.1 跨组中继数据不通（生产 08:11 回归）

拓扑有边、直连 session 正常、两跳路由失败。机制链如下（全部可对应到具体提交）：

1. **ace2669 / 0997491：泛洪副本不再刷新 `last_update`（保留源头时间戳）**。
   `update_peer_infos()` 里上游是"每收到一份就盖本地 now"，dokitier 改成了"保留 origin 的
   last_update，只在版本更高时保留较大值"。后果：条目新鲜度不再由泛洪维持，只由源头
   自身的刷新节奏（`UPDATE_PEER_INFO_PERIOD = 3600s` 或本地变更）决定。

2. **49c8f01：GC 大幅收紧**：`REMOVE_DEAD_PEER_INFO_AFTER` 3660s→300s、
   `REMOVE_UNREACHABLE_PEER_INFO_AFTER` 90s→10s、扫描周期 60s→5s。

3. 两者相乘 = **死锁式矛盾**：条目最长 3600s 才被源头刷新一次，但任何节点在 300s 后就把它
   硬删、10s+拓扑不可达就提前删。全网路由表周期性蒸发，`next_hop_map` 随之重建，
   表现为"直连 session 还活着（不受路由表影响），中继路径整段消失"。这就是 08:11 后
   HKBN→ix-sz 通但 →cn 不通、126.3 ping 得到 126.254 却 ping 不到 overseas 的直接机制。

4. **叠加伤害**（同一批提交，全部要回退）：
   - `version = max(old+1, unix秒)`：混用计数器与墙钟，节点间 30s 时钟差即可造成
     "新的版本更小 → LSA 被拒"；重启后与邻居存量版本比较方向不定。
   - `try_update_new_peer_info` 里的"同 instance 转世仲裁"块：按 last_update/version
     对称驱逐、删除对方 `conn_map`/`raw_peer_infos`、`continue` 丢弃入站——三个状态
     （peer_infos / conn_map / dst_saved 水位）被非原子地改动，路由图与转发面脱节。
   - OSPF 同步循环删掉 `last_sync_succ_timestamp` 断点、改全表遍历：每轮消息体积膨胀，
     与水位机制语义不再对齐。

### 1.2 僵尸节点（重启后旧实例长期/永久残留）

协议层缺口，不是参数问题：

- `OspfRouteRpc` 只有 `SyncRouteInfo` 一个 RPC。**协议里没有任何"撤销/驱逐"消息**，
  条目消失只能靠各节点本地超时（GC）。
- 各节点超时阈值与起点不同步，且**没有防复活机制**：蛛网中任意一个仍持有旧条目的节点，
  会把它重放给任何 ack 水位不覆盖该条目的会话（新建 session、session id 变化清水位、
  对端刚 GC 掉自己那份）→ 对端复活 → 再泛洪 → 无限乒乓。网越密越死不干净。
- d22f8f1 的"稳定 peer_id"只是把症状从"两个 peer 条目"挪到"同 peer_id 双 incarnation"，
  为此引入的仲裁逻辑反而制造了 1.1 的回归。身份耦合是错的解法方向。

### 1.3 p2p_group 语义（对照上游 need_p2p/disable_p2p）

用户确认的标准语义：

> p2p_group 等价于 easytier 原生"需要 p2p / 禁止 p2p"那一层开关，只作用于
> **自动发起的 p2p 连接**；不限制路由、不限制转发、不影响手动配置的 peer、
> 不影响 listener 接受的入站连接。跨组节点之间唯一的自动连接方式是"配置文件里
> 明确写的手动 peer"；A 手动只写了 B，绝不能让 A 自动连上 B 之外（尤其别的组）的节点。

现状：判定点只有两处——`connectivity/direct/mod.rs:347`（direct 连接候选过滤）与
`connectivity/hole_punch/peer_adapters.rs:47`（打洞候选过滤），核心规则
`is_cross_group_for()` 本身正确。缺的是**审计与闭环**：
- direct 候选收集是否覆盖所有自动发起入口（含经由 route info 学到的 peer）；
- 手动 peer 路径必须完全绕过组过滤；
- 入站（listener origin）一律不因组被拒；
- 转发/路由层零引用组字段（`peer_conn.rs` 只随握手携带用于展示，需保持）。

### 1.4 与 RFC 2328 OSPF 的差距总表

| 机制 | RFC 2328 | 当前实现 | 差距定性 |
|---|---|---|---|
| 死亡检测 | Hello + RouterDeadInterval，邻居主动判死 | 连接层 ping/pong + 连接关闭（秒级，`peer_conn.rs` pinger） | 能力已有，未被路由层利用 |
| 变化宣告 | LSA 立即重生成 + 泛洪 + 重传到确认 | conn/peer info 变更 → version+1 → 同步循环 + 每会话 ack 水位 | 基本等价 |
| 周期刷新 | LSRefreshTime 30min | 3600s | 等价 |
| **撤销/驱逐** | **LSA 可提前老化（MaxAge）并泛洪，收到即删并继续转发** | **无任何撤销消息，只能超时蒸发** | **根本缺口（僵尸根源）** |
| 防复活 | 序列号单调；MaxAge 优先于一切旧实例 | version 重启清零、无墓碑 | 旧条目可无限复活 |
| 老化 | MaxAge 1h 硬顶 | dokitier 300s/10s 且配合源头时间戳 | 活条目被误杀（中继回归根源） |
| 泛洪确认 | 显式 ack + 重传列表 | dst_saved_*_version 水位 | 近似等价 |

---

## 二、修改方案

### 第 1 步：数据面回退上游基线（恢复中继可达）

文件 `crates/dokitier-core/src/peers/route/peer_ospf_route.rs`：

1. `update_peer_infos()`：删除"转世驱逐 + 保留源头 last_update + route_id 变更时抬版本"
   三个块，恢复上游两行语义：
   - `route_info.last_update = Some(Timestamp::now());`（收到即刷新）
   - `version > old.version` 才接受。
2. `update_my_peer_info()` / `update_my_conn_info()`：恢复 `version = old + 1`
   （删除 unix 秒 max）。
3. `my_peer_route_id`：恢复纯 `rand::random()`（上游逐字节一致）。
4. 同步循环 `collect_peer_infos` / `collect_conn_infos`：恢复
   `last_sync_succ_timestamp` 断点 + `.rev()` 迭代（上游逐字节一致）。
5. GC 常量：`3660s / 90s`、扫描 60s（先恢复上游值；第 2 步落地墓碑后再收紧不可达窗口）。
6. `check_duplicate_peer_id` 的 panic 与上游一致，保留（随机 peer_id 下碰撞概率可忽略）。

文件 `crates/dokitier-core/src/peers/peer_manager.rs`：

7. `my_peer_id` 恢复上游 `random_peer_id()`，删除 `peer_id_from_instance_id` 派生。
   `instance_id` 继续保留并随运行时留存（console 映射、诊断用），但不再参与路由身份。

保留不动（dokitier 自有功能，不触碰状态机）：p2p_group 字段透传、静态路由
（link_policy）、带宽探测 RPC、`list_conn_snapshots`。

### 第 2 步：撤销 + 墓碑（对应 RFC MaxAge-flush，根治僵尸）

**proto**（`crates/dokitier-proto/proto/peer_rpc.proto`）：
`SyncRouteInfoRequest` 与 `SyncRouteInfoResponse` 各新增
`repeated PeerIdVersion withdrawn_peer_infos = N;`（新字段号，向后兼容）。

**状态**（`SyncedRouteInfo`）：
`withdrawn_peer_infos: RwLock<OrderedHashMap<PeerId, (Version, Instant)>>`
——peer_id → 撤销版本 + 学习时间（TTL 120s，`clear_expired_peer` 扫描时过期清理）。

**产生**：
- 优雅退出：路由服务 `stop()` 时，把自己的条目写入撤销表（version = 当前+1），
  对每个存活 session 强制补发一轮同步后再关闭连接。
- 崩溃/kill -9：无优雅消息。邻居在秒级感知断连（现有 pinger）→ conn_map 更新并泛洪
  （现有机制）→ 全网拓扑重算为不可达 → 不可达 GC 窗口收紧为 **15s**
  （`REMOVE_UNREACHABLE_PEER_INFO_AFTER` = 15s，仅在第 2 步墓碑落地后执行）
  → 删除条目时**同时写墓碑**。15s 窗口是链路抖动与真死的区分窗口；期间转发本来就
  已不可达，影响的只是条目清理时刻。

**传播与防复活**：
- 撤销表作为同步消息的一部分随每轮 sync 双向携带（TTL 内始终携带，量小），
  复用现有每会话 ack 水位的可靠性。
- 接收侧 `update_withdrawn_peer_infos()`：登记墓碑（版本取 max）、删除该 peer 的
  peer_infos / conn_map / raw_peer_infos 条目。
- **接受守卫**：`update_peer_infos` / conn 更新在接受前先查墓碑，
  `version <= tombstone` 的一律拒绝且**不入表不转发**——蛛网重放复活路径被彻底切断。
- 墓碑 TTL 120s 过期后自然清除；因 peer_id 为每进程随机，重启的新实例是新 peer_id，
  旧墓碑对它零影响。

### 第 3 步：p2p_group 标准语义闭环

1. 保持 `is_cross_group_for()` 既有规则（空组/无组一律放行）。
2. 审计并补齐 direct 连接器所有自动候选入口的过滤（含 route info 学到的 peer）。
3. 手动 peer（配置文件 peers 段）连接路径不经过组过滤——代码路径确认 + 注释钉死。
4. listener 入站连接不做组拒绝。
5. 路由/转发层确认零依赖组字段（现状已是，加回归测试锁住）。

### 重启场景时序（验收目标）

- **优雅关闭老进程**：stop → 撤销广播 → 各节点秒级删除 + 墓碑 → 旧实例全网消失（≤ 一个
  同步周期，约 1–5s）；新进程启动即新身份，正常建连转发。
- **kill -9**：断连秒级感知 → 边撤销泛洪（~1–2s）→ 拓扑不可达 15s → 墓碑删除，
  全网消失 ≤ ~20s，之后不可能被重放复活。
- 两种场景下 `easytier-cli peer` / console 拓扑都不再出现重复节点。

---

## 三、验证计划（仅 CI / 测试环境，不碰生产）

1. `cargo test -p dokitier-core`（路由相关测试逐个过，含上游移植的单测）。
2. CI 4 组链式拓扑（组1-组2-组3-组4，全节点带组，组间仅手动 peer）：
   - 任意两节点 ping/数据互通（p2p 或 relay）；
   - 断开任一中继，路径自动收敛。
3. 重启测试：容器内 kill -9 与优雅 stop 两种，断言旧 peer 条目在其余所有节点
   路由表中的消失时间上限；重启后立即验证转发。
4. p2p_group 回归：同组自动 p2p 建立；跨组仅手动 peer 通；A 只手动写 B，
   断言 A 不自动连其它组节点；转发与组无关。
5. 混跑注意：撤销字段为新增 optional，旧节点忽略新字段——但本次**生产混跑期间
   不部署**，先在 CI 全新版本内验证。

## 四、约束

- 生产只读；不推送 GitHub、不触发 Actions；SSH 仅本机发起。
- 所有代码改动完成后交 Lead 审查 diff 并跑测试，未经确认不部署。

---

## v3 修复：墓碑自删死锁（12:28 版本引出，12:35 修复）

滚动部署期间 126.3 上实锤的四个复合缺陷与修复：

| # | 缺陷 | 修复 |
|---|---|---|
| A | 启动期拓扑未成形，GC 可把**自己的行** GC+墓碑化 | clear_expired_peer 跳过 self |
| B | 墓碑 TTL 被转发副本不断续期 → 94 条永生墓碑全网互相刷新 | TTL 锚定首次登记时间，转发不续期；仅 by_origin 升级时重置 |
| C | 节点收到含自己 peer_id 的转发墓碑 → 无条件删光自己全部行 | withdraw_peer_infos 忽略 self 的墓碑（签名加参） |
| D | 邻居申报墓碑无条件删行，可误杀更新的 incarnation | 仅删 ≤ 下限的行 |

存量永生墓碑（过期时间已过去）部署后立即自然过期，无需人工清理。

## v4：路由代价多模式框架（2026-11-10）

### 模式
| 模式 | 语义 |
|---|---|
| **Smart（智能，默认）** | 每跳基础代价（延迟+丢包+跳数权重，自身路由流量不计入链路占用）+ 瓶颈带宽惩罚每路径一次（路径可用带宽 = 链路可用容量最小值，分层限制 Dijkstra 精确求解）；路径粘滞 20%（旧路径在当前图端到端重验后才允许被替换） |
| **MinLatency（最低延迟）** | 纯链路延迟求和 |
| **LeastHop（最少跳数）** | 跳数计数（沿用既有 hop 表） |

### 模块化
加新模式 = `route/cost_mode.rs` 加枚举变体 + `peer_ospf_route.rs` 加一个 `gen_next_hop_map_*`，数据面零改动。

### 链路占用归属（防震荡的关键语义）
- 数据面按 (下一跳链路, 最终目的地) 计数每个转发字节（分片 DashMap + Atomic，热路径零全局锁、零额外路由查询）
- Smart 模式对每个目的地 D 独立计算：链路可用容量 = 容量 − 其他目的地流量；**D 自身的流量不计入自身选路**（d-z 的 200m 在 a-c 选路时计入 b 的链路，d-z 自身选路时扣除）——从机理上消除 a-b-c ↔ a-d-c 循环震荡
- 本地链路用自身计数器；远端链路经 peer_center 传播 per-dst 速率（`DirectConnectedPeerInfo.dst_rates_mbps`，1Mbps 量化保 digest 稳定）
- 速率采样在路由重算时同步执行（控制面节奏），无后台任务触碰数据面计数器

### 选择与热更新
console 系统设置 → 路由模式三选一 → `SetRoutingWeights` 命令热推所有在线节点 → generation 递增触发重算。节点本地 TOML `[routing] mode` 与 node-local WebUI 同样可配。

## 运维规则（生产验证结论，2026-11-10 12:40）

**版本切换/修复部署一律全量同时重启，不做滚动重启。**

原因（有实机证据）：路由状态机、墓碑、版本水位全部是进程内存态。滚动重启期间新旧
二进制与多代内存状态混跑，错误时代的墓碑/水位/版本语义持续在网内流转和互相污染
（11:50 滚动重启后墓碑自删死锁全网扩散即此机理）。全量同时重启让所有节点从同一
干净状态起步，旧错误信息随旧进程一起消失。12:28 修复版全量重启后 ~2 分钟内全网
25/25 收敛，跨组 relay ping 恢复（126.2→127.1 三跳 54ms）。

注：v2 摘要反熵 + v3 墓碑修复落地后，协议本身已具备自愈能力（单节点重启/掉线不影响
全网）；"全量重启"是版本切换期的可靠部署策略，而非运行期依赖。

## v5：重启期身份持久化 + 版本单调性（2026-11-10 16:50）

### 实机定位（YaoVXLAN journalctl，2026-11-10 15:00–16:45）
- `my_peer_id` 随每次进程重启重掷：354857337 → 3915856643 → 3610205897 →
  4257617629 → 2136080158 → 1893771952 → 1561737800 → 4054056574（8 次）。
- console 运行时改配置（16:39:25，无进程重启的热重建）身份保持不变 —— registry
  修复已生效；**产生僵尸的是"改配置 → 服务重启"路径**：新进程掷新 peer_id，旧
  化身行全网残留直至墓碑/TTL 收割，表现为"改配置后多出僵尸节点，一段时间后消失"。
- 16:24 时段全网 path_latency = 500×跳数（每跳恒 500）：peer_center 全局 map 为空
  （旧版 2.7.0 节点 127.9 未替换污染同步），`RouteCostCalculatorImpl` 回退
  `(500ms, 0%)`。全量替换后 16:45 实测代价恢复正常（直连 2=2.76ms、9=9.85ms，
  多跳按链路求和），Smart 模式生效。**恒 500 代价是旧版本节点在网内的症状，不是
  代码缺陷。**

### 修复（三层，缺一即复发）
1. **身份持久化**（peer_manager.rs `stable_process_peer_id`）：未配置 peer_id 时
   改为从持久化 instance_id 派生（FNV-1a32），进程重启保持同一身份（router-id
   语义）。旧化身行被同身份刷新而非残留，僵尸类别从根源消除。
2. **版本单调性**（peer_ospf_route.rs）：自身 peer_info/conn_info 版本计数器
   （old+1）以 unix 秒为下限（`self_version_floor`）。持久身份下版本计数器随重启
   归零，不加下限则全网拒绝重启后全部通告（version ≤ 存量即拒）。外网路径此前已
   有同款下限，此处对齐。
3. **自重复检测豁免**（`check_duplicate_peer_id`）：重启后邻居转发的旧化身行
   （route_id 不同、版本高）会在首次路由 tick 前命中"我的 peer_id 被别人宣告"
   判定并 panic。加 `version > self_version_floor()` 守卫：旧化身行版本 ≤ 当前
   秒下限必然被下一次通告覆盖，不是重复；真重复节点时钟持续走高仍会被检出。

### 语义说明
- 真正新配置（新 instance_id）或显式配置 peer_id 才改变身份；instance_id 由
  console 生成并持久化在 TOML，天然唯一。
- 一次性迁移：本次部署全量同时重启后，各节点旧随机身份作为普通下线节点被墓碑
  收割一次；此后重启/改配置不再产生任何僵尸行。

## v6：端到端单路径钉扎（2026-11-10 17:30）

### 现象
a→b 存在多条可用路径时，流量被分摊到多条路径同时跑，而不是钉在一条。

### 定位（代码级，非路由层）
- 路由层无分摊：next_hop_map 每目的地单下一跳，`send_via_next_hop` 单路径转发。
- 分摊发生在**连接层**（来自本仓库 1c3d62a，非上游）：
  1. `Peer::send_msg` 对每个数据包解析五元组 flow_key，按流绑定并行连接
     （`select_flow_conn` → `FlowBindings`，绑定存活 IDLE_TIMEOUT）；
  2. `default_conn_clear_task` **每 5 秒清除 default_conn**（只要同 peer 并行连接
     数 >1），于是每个新流重新执行 `select_conn()`（按测量 RTT 选），不同时刻创建
     的流落在不同并行连接上 → 同一对节点间的流量按流散到多条路径。

### 修复（peer.rs）
1. 移除 5 秒周期性清除：default_conn 钉死直到该连接死亡；close 监听器本就只在
   被钉连接关闭时清 pin，下个包重选。流绑定语义自然收敛（新流全部绑到被钉连接）。
2. `conn_selection_key` 加 conn_id 兜底并列：DashMap 迭代序不稳定，等价连接
   （如两条均未测距）重选时会随机换人，必须确定性决胜。

### 语义
- 一对节点间的全部流量固定走一条实测最优连接，直到该连接断开。
- 新建更优连接（如后建立的打洞直连）不会抢占存量流量；要换路径 = 断开或手动关旧
  连接。如需"显著更优才迁移"的滞后策略，后续可在 5s 任务里加 2x RTT 判定。

## v7：上下行同路径（对称选路，2026-11-10 17:50）

### 现象
a→b 与 b→a 走不同中继路径（上下行不对称）。

### 根因（OSPF 类选路的经典问题）
每个节点独立算路，路径对称需要三件事同时成立，此前全部缺失：
1. **边代价不对称**：SmartEdge 按有向边建，两个方向各自读自己视角的延迟/占用，
   且 `link_capacity_mbps(u,v)=min(up_u, down_v)` 本身随方向变；自己的链路还用
   本地实时计数器、别人用报告 —— 两端算出不同代价 → 选不同路径。
2. **各端独立粘滞**：20% 滞回带内各端各自保留自己的历史路径，一旦分叉永不收敛。
3. 等代价平局随机破。

### 修复（全部在控制面选路，数据面零改动）
1. **链路对称合并**（build_peer_graph_from_snapshot）：按无向对收集两端报告，
   每条链路只产出一条规范边：延迟/丢包取两方向报告的最差值、流量取较差方向的
   max-union、avoid_relay 保留方向语义。新增 trait `link_metrics`(严格本端报告,
   无 500 兜底),Option 语义区分"未上报"与"上报 500ms"。
2. **双向同权展开**（expand_symmetric_weighted_edges）：Dijkstra 边表 u→v 与
   v→u 权重完全相同；容量取 min(两方向视图)。
3. **占用输入统一为泛洪报告**：删除"自己链路用本地计数器"分支 —— 本地计数器
   只喂数据面统计与上报，选路一律用 peer_center 泛洪快照（两端看到同一份输入，
   这是对称性的必要条件）。
4. **规范路径粘滞**（install_mode_candidate + canonical_path_rank）：滞回带内
   不再"保留旧路径"，而是比较两候选路径的"排序中间节点序列"（翻转不变量：
   a-r1-x-b 与 b-x-r1-a 同秩），两端一致选中较小者 → 正反向收敛到同一物理路径。
   带外仍按代价切，死路径仍立即切换。
5. graph_algo 增加 `dijkstra_peer_edges_with_paths`（返回完整路径，供秩比较）。

### 已知边界
- 自动探测容量（BPROBE）是各端本地测量、未泛洪；节点带宽已配置时容量来自泛洪
  带宽（对称）。纯依赖自动探测且未配置节点带宽时容量项仍可能轻微不对称（延迟
  项已对称，主代价不受影响）。
- 平局（完全等代价路径）由 canonical_path_rank 统一裁决。

## v8：去中心化链路指标泛洪（peer-center 退役前置，2026-11-10 21:20）

### 背景
Smart 模式代价的输入（直连链路延迟/丢包/带宽）依赖 peer-center 聚合点
（min peer id 当聚合器，digest 增量拉取）。聚合点链路一卡或节点自身同步
任务死锁（共享锁 + 无超时 RPC），单节点全盲、代价回退 500/跳。

### 方案：指标随 OSPF LSA 泛洪（与路由同可靠性等级）
1. **数据源**：conn 层 ping-pong 每轮把实测 (latency_ms, loss_perm) 推入
   `link_policy::DIRECT_CONN_METRICS`（分片 map，60s TTL，非数据路径）。
2. **泛洪**：`RoutePeerInfo` 新增 `direct_conn_metrics`（邻居→延迟/丢包，
   ms/千分位量化）+ 节点带宽上下行。内容变化 → 自动版本递增 → 走现有
   OSPF 可靠同步（ack 水位/摘要反熵/raw 字节透传——中继不解析也能转发，
   旧版本节点字节级兼容）。量化保证延迟抖动不引发同步风暴。
3. **消费**：`RouteCostCalculatorImpl::directed_info` 优先读本地路由表的
   泛洪指标（内存读、零请求、零延迟），peer-center 降级为混合版本舰队
   的回退源；节点带宽同理（泛洪值覆盖 peer-center 值）。
4. **peer-center watchdog**（上轮）：report/fetch 任务 60s 硬超时 + tick
   探针，共享锁死锁类别自愈。

### 兼容性
- proto 新字段：旧版本节点字节级透传（raw bytes 不解析），新节点解析；
  旧节点自身不发布指标 → 其链路代价继续走 peer-center 回退。
- peer-center 模块保留（带宽上报、混合版本回退），待全舰队 ≥本版本后
  可评估退役。

## v9：OSPF 2-Way 邻接校验（2026-11-11）

### 症状（实机）
- 全量重启后全网通几秒，随后部分节点（日本/海外链）不通；单节点重启后 relay 丢包数分钟。
- 链路追踪：cn↔海外存在**单向半死连接**（us-oracle 桥：conn 存活、数据单向黑洞），OSPF 图仍把它当可用边，代价一低就被选中 → 流量灌入黑洞。

### 修复
`build_peer_graph_from_snapshot` 强制 **OSPF 2-Way**：边 (u,v) 只有在 u、v 双方的 conn_map **当前都互相宣告**时才进图。单侧残留的陈旧 conn_info（半死隧道、重启后未清理的旧邻接）不再构成边。
