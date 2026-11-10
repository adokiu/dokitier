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

## 运维规则（生产验证结论，2026-11-10 12:40）

**版本切换/修复部署一律全量同时重启，不做滚动重启。**

原因（有实机证据）：路由状态机、墓碑、版本水位全部是进程内存态。滚动重启期间新旧
二进制与多代内存状态混跑，错误时代的墓碑/水位/版本语义持续在网内流转和互相污染
（11:50 滚动重启后墓碑自删死锁全网扩散即此机理）。全量同时重启让所有节点从同一
干净状态起步，旧错误信息随旧进程一起消失。12:28 修复版全量重启后 ~2 分钟内全网
25/25 收敛，跨组 relay ping 恢复（126.2→127.1 三跳 54ms）。

注：v2 摘要反熵 + v3 墓碑修复落地后，协议本身已具备自愈能力（单节点重启/掉线不影响
全网）；"全量重启"是版本切换期的可靠部署策略，而非运行期依赖。
