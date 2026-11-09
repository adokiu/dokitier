<template>
  <div>
    <div class="d-flex align-center mb-3 flex-wrap gap-3">
      <div>
        <div class="page-title">{{ t('nodes.title') }}</div>
        <div class="page-sub">{{ t('nodes.subtitle') }}</div>
      </div>
      <v-spacer />
      <v-btn color="primary" size="small" prepend-icon="mdi-plus" @click="openAddNode">{{ t('nodes.addNode') || '添加节点' }}</v-btn>
      <v-select
        v-model="networkFilter"
        :items="networkFilterItems"
        item-title="title"
        item-value="value"
        :label="t('nodes.networkFilter')"
        density="compact"
        variant="outlined"
        hide-details
        style="max-width: 240px"
      />
    </div>

    <v-card>
      <DokTable
        class="nodes-table"
        :headers="headers"
        :items="filteredNodes"
        :items-per-page="20"
        :loading="loading"
        item-value="node_id"
        hover
        @click:row="(_, row) => open(row.item)"
      >
        <template #item.status="{ item }">
          <v-chip size="x-small" :color="item.status === 'online' ? 'success' : 'error'" variant="tonal">{{ item.status }}</v-chip>
        </template>
        <template #item.alias="{ item }">
          <div class="d-flex align-center gap-2 node-name">
            <img :src="flagUrl(item)" class="node-flag" alt="" loading="lazy" />
            <span class="node-alias">{{ item.alias || item.hostname || item.node_id }}</span>
          </div>
        </template>
        <template #item.vpnIp="{ item }">
          <div class="ip-cell">
            <div v-if="ipInfo(item, 'vpn').v4">{{ ipInfo(item, 'vpn').v4 }}</div>
            <v-tooltip v-if="ipInfo(item, 'vpn').v6" location="top" :text="ipInfo(item, 'vpn').v6full">
              <template #activator="{ props }">
                <div v-bind="props" class="ip-v6">{{ ipInfo(item, 'vpn').v6 }}</div>
              </template>
            </v-tooltip>
            <div v-if="!ipInfo(item, 'vpn').v4 && !ipInfo(item, 'vpn').v6">-</div>
          </div>
        </template>
        <template #item.publicIp="{ item }">
          <div class="ip-cell">
            <div v-if="ipInfo(item, 'public').v4">{{ ipInfo(item, 'public').v4 }}</div>
            <v-tooltip v-if="ipInfo(item, 'public').v6" location="top" :text="ipInfo(item, 'public').v6full">
              <template #activator="{ props }">
                <div v-bind="props" class="ip-v6">{{ ipInfo(item, 'public').v6 }}</div>
              </template>
            </v-tooltip>
            <div v-if="!ipInfo(item, 'public').v4 && !ipInfo(item, 'public').v6">-</div>
          </div>
        </template>
        <template #item.cpu="{ item }">
          <div class="meter-cell">
            <div class="meter-value">{{ pctText(item.cpu_pct) }}</div>
            <div class="meter-track"><div class="meter-fill" :style="{ width: barWidth(item.cpu_pct), background: usageColor(item.cpu_pct) }" /></div>
          </div>
        </template>
        <template #item.mem="{ item }">
          <div class="meter-cell">
            <div class="meter-value">{{ pctText(memPct(item)) }}</div>
            <div class="meter-track"><div class="meter-fill" :style="{ width: barWidth(memPct(item)), background: usageColor(memPct(item)) }" /></div>
          </div>
        </template>
        <template #item.version="{ item }"><span class="text-caption version-cell">{{ item.version || '-' }}</span></template>
        <template #item.traffic="{ item }">
          <div class="traffic">
            <span class="up">↑ {{ bps(item.tx_bps) }}</span>
            <span class="down">↓ {{ bps(item.rx_bps) }}</span>
          </div>
        </template>
        <template #item.trafficTotal="{ item }"><span class="traffic-total">{{ humanBytes(trafficTotal(item)) }}</span></template>
        <template #item.actions="{ item }">
          <div class="d-flex gap-1">
            <v-btn size="x-small" variant="text" prepend-icon="mdi-cog" @click.stop="open(item)">{{ t('nodes.config') }}</v-btn>
            <v-btn size="x-small" variant="text" prepend-icon="mdi-download" @click.stop="openInstall(item)">安装</v-btn>
          </div>
        </template>
      </DokTable>
    </v-card>

    <DetailDrawer v-model="drawer" :title="detail ? (detail.hostname || detail.node_id) : ''" :subtitle="detail ? (detail.instance_ids?.[0]?.instance_id || detail.node_uuid || detail.node_id) : ''" :width="620">
      <template v-if="detail">
        <v-tabs v-model="drawerTab" density="compact" class="drawer-tabs mb-3">
          <v-tab value="network">网络配置</v-tab>
          <v-tab value="node">节点配置</v-tab>
          <v-tab value="system">节点参数</v-tab>
        </v-tabs>

        <div class="drawer-panels">
          <div v-show="drawerTab === 'network'">
            <div class="stat-label mb-2">{{ t('nodes.networks') }} ({{ currentNetworks.length }})</div>
            <div class="d-flex align-center gap-2">
              <v-select
                :model-value="network"
                :items="networkSelectItems"
                item-title="title"
                item-value="value"
                label="选择网络"
                density="compact"
                variant="outlined"
                hide-details
                class="network-picker"
                @update:model-value="selectNetwork"
              />
              <v-btn size="small" variant="text" prepend-icon="mdi-plus" @click="addNetworkOpen = true">添加网络</v-btn>
              <v-btn v-if="network" size="small" variant="text" icon="mdi-delete-outline" @click="deleteNetwork(network)" />
            </div>

            <template v-if="network">
              <v-divider class="my-4" />
              <div class="d-flex align-center mb-2">
                <span class="stat-label">{{ t('nodes.configFile') }} / {{ network }}</span>
                <v-spacer />
                <v-btn size="x-small" variant="text" prepend-icon="mdi-history" @click="openVersions">{{ t('nodes.history') }}</v-btn>
                <v-btn size="x-small" variant="text" prepend-icon="mdi-download" @click="downloadConfig">{{ t('nodes.download') }}</v-btn>
              </div>
              <v-alert v-if="draftNetwork" type="info" density="compact" variant="tonal" class="mb-3">
                新网络草稿（{{ draftNetwork }}）：只有点击“保存并应用”才会下发，不会自动提交。
              </v-alert>
              <NetworkConfigEditor ref="configEditor" v-model:toml="configText" />
              <v-alert v-if="configError" type="error" variant="tonal" density="compact" class="mb-2">{{ configError }}</v-alert>
              <v-alert v-if="saved" type="success" variant="tonal" density="compact" class="mb-2">{{ t('nodes.applied') }}</v-alert>
              <div class="d-flex gap-2 config-actions">
                <v-btn color="primary" :loading="saving" @click="saveConfig">{{ t('nodes.saveApply') }}</v-btn>
                <v-btn variant="text" :loading="configLoading" @click="loadConfig">{{ t('nodes.reload') }}</v-btn>
              </div>
            </template>
          </div>

          <div v-show="drawerTab === 'node'">
            <div class="node-settings">
              <div class="stat-label mb-1 d-flex align-center">
                节点名称
                <v-tooltip location="top" text="留空显示主机名，用于列表和拓扑展示">
                  <template #activator="{ props }"><v-icon v-bind="props" size="14" class="ml-1 info-icon">mdi-information-outline</v-icon></template>
                </v-tooltip>
              </div>
              <v-text-field v-model="nodeAlias" density="compact" variant="outlined" hide-details placeholder="留空显示主机名" class="mb-4" />
              <div class="stat-label mb-1 d-flex align-center">
                上行带宽（Mbps）
                <v-tooltip location="top" text="节点上行能力，参与链路容量 min(up(src), down(dst))">
                  <template #activator="{ props }"><v-icon v-bind="props" size="14" class="ml-1 info-icon">mdi-information-outline</v-icon></template>
                </v-tooltip>
              </div>
              <v-text-field v-model.number="nodeBandwidthUp" type="number" density="compact" variant="outlined" hide-details placeholder="例如 500" class="mb-4" />
              <div class="stat-label mb-1 d-flex align-center">
                下行带宽（Mbps）
                <v-tooltip location="top" text="节点下行能力，作为对端看到的链路容量上限">
                  <template #activator="{ props }"><v-icon v-bind="props" size="14" class="ml-1 info-icon">mdi-information-outline</v-icon></template>
                </v-tooltip>
              </div>
              <v-text-field v-model.number="nodeBandwidthDown" type="number" density="compact" variant="outlined" hide-details placeholder="例如 100" class="mb-4" />
              <div class="stat-label mb-1 d-flex align-center">
                内网下行上限（Mbps）
                <v-tooltip location="top" text="对端为内网地址时额外施加的容量上限，留空继承下行带宽">
                  <template #activator="{ props }"><v-icon v-bind="props" size="14" class="ml-1 info-icon">mdi-information-outline</v-icon></template>
                </v-tooltip>
              </div>
              <v-text-field v-model.number="nodeIntranetCap" type="number" density="compact" variant="outlined" hide-details placeholder="留空使用下行带宽" class="mb-4" />
              <div class="stat-label mb-1 d-flex align-center">
                共享监听地址（core 级）
                <v-tooltip location="top" text="所有网络共用；保存后原子热更新，已有连接不断开。可添加多个监听（TCP 与 QUIC 可使用同一端口）。">
                  <template #activator="{ props }"><v-icon v-bind="props" size="14" class="ml-1 info-icon">mdi-information-outline</v-icon></template>
                </v-tooltip>
              </div>
              <div v-for="(l, i) in nodeSharedListeners" :key="'sl' + i" class="shared-listener-row">
                <v-select v-model="l.proto" :items="sharedProtoOptions" density="compact" variant="outlined" hide-details class="proto-select" />
                <v-text-field v-model="l.addr" density="compact" variant="outlined" hide-details placeholder="0.0.0.0:12401" />
                <v-btn size="small" variant="text" icon="mdi-delete-outline" @click="nodeSharedListeners.splice(i, 1)" />
              </div>
              <v-btn size="small" variant="text" prepend-icon="mdi-plus" class="mb-4" @click="nodeSharedListeners.push({ proto: 'tcp', addr: '0.0.0.0:12401' })">添加共享监听</v-btn>
              <div class="stat-label mb-1 d-flex align-center">
                公网下行上限（Mbps）
                <v-tooltip location="top" text="对端为公网地址时额外施加的容量上限，留空继承下行带宽">
                  <template #activator="{ props }"><v-icon v-bind="props" size="14" class="ml-1 info-icon">mdi-information-outline</v-icon></template>
                </v-tooltip>
              </div>
              <v-text-field v-model.number="nodeInternetCap" type="number" density="compact" variant="outlined" hide-details placeholder="留空使用下行带宽" class="mb-4" />
              <v-alert v-if="nodeSaveError" type="error" variant="tonal" density="compact" class="mb-3">{{ nodeSaveError }}</v-alert>
              <v-btn color="primary" :loading="savingNodeSettings" @click="saveNodeSettings">保存并应用</v-btn>
            </div>
          </div>

          <div v-show="drawerTab === 'system'">
            <div class="sys-grid">
              <div class="sys-card">
                <div class="sys-title">CPU</div>
                <div class="sys-main">{{ sysInfo.cpu_model || '-' }}</div>
                <div class="sys-sub">{{ sysInfo.cpu_cores || '-' }} 核 / {{ sysInfo.cpu_threads || '-' }} 线程 / {{ sysInfo.cpu_arch || '-' }}</div>
              </div>
              <div class="sys-card">
                <div class="sys-title">内存</div>
                <div class="sys-main">{{ fmtBytes(detail.mem_total) }}</div>
                <div class="sys-sub">已用 {{ fmtBytes(detail.mem_used) }} / {{ Number(detail.mem_pct || 0).toFixed(1) }}%</div>
                <div class="sys-bar"><div class="sys-bar-fill" :style="{ width: Math.min(100, Number(detail.mem_pct || 0)) + '%' }" /></div>
              </div>
              <div class="sys-card sys-card--wide">
                <div class="sys-title">系统</div>
                <div class="kv"><span>主机名</span><b>{{ sysInfo.hostname || detail.hostname || '-' }}</b></div>
                <div class="kv"><span>操作系统</span><b>{{ sysInfo.os_version || sysInfo.os_name || '-' }}</b></div>
                <div class="kv"><span>内核</span><b>{{ sysInfo.kernel || '-' }}</b></div>
                <div class="kv"><span>运行时长</span><b>{{ uptimeText }}</b></div>
              </div>
              <div class="sys-card sys-card--wide">
                <div class="sys-title">公网与路由</div>
                <div class="kv"><span>公网 IPv4</span><b class="num">{{ detail.public_ipv4 || '未检测到' }}</b></div>
                <div class="kv"><span>公网 IPv6</span><b class="num">{{ detail.public_ipv6 || '未检测到' }}</b></div>
                <div class="kv"><span>网关</span><b class="num">{{ sysNetwork.gateway ? `${sysNetwork.gateway.gateway} dev ${sysNetwork.gateway.interface}` : '未检测到' }}</b></div>
                <div class="kv"><span>IPv4 地址</span><b class="num">{{ ipv4List || '未检测到' }}</b></div>
                <div class="kv"><span>IPv4 段</span><b class="num">{{ (sysNetwork.ipv4_prefixes || []).join('  ') || '未检测到' }}</b></div>
                <div class="kv"><span>IPv6 地址</span><b class="num">{{ ipv6List || '未检测到' }}</b></div>
                <div class="kv"><span>IPv6 段</span><b class="num">{{ (sysNetwork.ipv6_prefixes || []).join('  ') || '未检测到' }}</b></div>
              </div>
              <div class="sys-card sys-card--wide">
                <div class="sys-title">网卡</div>
                <div class="nic-scroll">
                  <table class="nic-table">
                    <thead><tr><th>网卡</th><th>状态</th><th>驱动/速率</th><th>MAC</th><th>IPv4</th><th>IPv6</th></tr></thead>
                    <tbody>
                      <tr v-for="nic in sysNetwork.interfaces || []" :key="nic.name">
                        <td>{{ nic.name }}</td>
                        <td>{{ nic.state || '-' }}</td>
                        <td>{{ nic.driver || '-' }}<span v-if="nic.speed_mbps"> / {{ nic.speed_mbps }} Mbps</span></td>
                        <td class="num">{{ nic.mac || '-' }}</td>
                        <td class="num">{{ (nic.ipv4 || []).join(' ') || '-' }}</td>
                        <td class="num">{{ (nic.ipv6 || []).join(' ') || '-' }}</td>
                      </tr>
                    </tbody>
                  </table>
                </div>
              </div>
              <div class="sys-card sys-card--wide">
                <div class="sys-title">磁盘</div>
                <div class="nic-scroll">
                  <table class="nic-table">
                    <thead><tr><th>设备</th><th>挂载点</th><th>类型</th><th>容量</th><th>可用</th></tr></thead>
                    <tbody>
                      <tr v-for="disk in sysInfo.disks || []" :key="disk.name + disk.mount">
                        <td>{{ disk.name }}</td>
                        <td class="num">{{ disk.mount || '-' }}</td>
                        <td>{{ disk.kind || '-' }}</td>
                        <td>{{ fmtBytes(disk.total) }}</td>
                        <td>{{ fmtBytes(disk.available) }}</td>
                      </tr>
                    </tbody>
                  </table>
                </div>
              </div>
              <div class="sys-card sys-card--wide">
                <div class="sys-title">隧道质量</div>
                <div class="nic-scroll">
                  <table class="nic-table">
                    <thead><tr><th>网络</th><th>类型</th><th>对端</th><th>Ping 延迟</th><th>Ping 丢包</th><th>传输丢包</th><th>容量</th><th>来源</th><th>利用率</th></tr></thead>
                    <tbody>
                      <tr v-for="row in tunnelRows" :key="row.network + row.remote">
                        <td>{{ row.network }}</td>
                        <td>
                          <span v-if="row.typeLabel !== '-'" class="proto-badge" :class="'proto-badge--' + row.typeClass" :title="row.typeTitle">{{ row.typeLabel }}</span>
                          <span v-else>-</span>
                        </td>
                        <td class="mono">{{ row.remote }}</td>
                        <td>{{ row.latency != null ? row.latency.toFixed(2) + ' ms' : '-' }}</td>
                        <td>{{ row.loss != null ? row.loss.toFixed(2) + ' %' : '-' }}</td>
                        <td>{{ row.transportLoss != null ? row.transportLoss.toFixed(2) + ' %' : '-' }}</td>
                        <td>{{ row.capacity ? row.capacity.toFixed(0) + ' Mbps' : '未测量' }}</td>
                        <td>{{ capacitySourceLabel(row.capacitySource) }}</td>
                        <td>{{ row.utilization != null ? (row.utilization * 100).toFixed(1) + ' %' : '-' }}</td>
                      </tr>
                      <tr v-if="!tunnelRows.length"><td colspan="9" class="text-medium-emphasis">暂无数据</td></tr>
                    </tbody>
                  </table>
                </div>
              </div>
              <div class="sys-card sys-card--wide">
                <div class="sys-title">虚拟网络能力</div>
                <div class="kv" v-if="sysInfo.shared_listener"><span>共享监听</span><b class="num">{{ sysInfo.shared_listener }}</b></div>
                <div class="cap-row">
                  <v-chip size="x-small" :color="sysInfo.capabilities?.tun ? 'success' : 'error'" variant="tonal">TUN {{ sysInfo.capabilities?.tun ? '可用' : '不可用' }}</v-chip>
                  <v-chip size="x-small" :color="sysInfo.capabilities?.tap ? 'success' : 'error'" variant="tonal">TAP {{ sysInfo.capabilities?.tap ? '可用' : '不可用' }}</v-chip>
                  <v-chip size="x-small" :color="sysInfo.capabilities?.vxlan ? 'success' : 'error'" variant="tonal">VXLAN {{ sysInfo.capabilities?.vxlan ? '可用' : '不可用' }}</v-chip>
                </div>
              </div>
              <div class="sys-card sys-card--wide">
                <div class="sys-title">网络与运行</div>
                <div class="kv"><span>公网 IPv4</span><b class="num">{{ detail.public_ipv4 || '-' }}</b></div>
                <div class="kv"><span>公网 IPv6</span><b class="num">{{ detail.public_ipv6 || '-' }}</b></div>
                <div class="kv"><span>节点 UUID</span><b class="num">{{ detail.instance_ids?.[0]?.instance_id || detail.node_uuid || detail.node_id }}</b></div>
                <div class="kv"><span>版本</span><b>{{ detail.version || '-' }}</b></div>
              </div>
            </div>
          </div>
        </div>
      </template>
    </DetailDrawer>

    <v-dialog v-model="versionsOpen" max-width="620">
      <v-card class="pa-2">
        <v-card-title>{{ t('nodes.history') }} / {{ network }}</v-card-title>
        <v-list density="compact">
          <v-list-item v-for="v in versions" :key="v.id">
            <v-list-item-title>{{ new Date(v.created * 1000).toLocaleString() }}</v-list-item-title>
            <v-list-item-subtitle class="num">{{ v.network }} / {{ v.hash.slice(0, 16) }}</v-list-item-subtitle>
            <template #append>
              <v-btn size="x-small" variant="text" :loading="rollingBack === v.id" @click="rollback(v)">回滚</v-btn>
            </template>
          </v-list-item>
          <v-list-item v-if="!versions.length"><v-list-item-title class="text-medium-emphasis">{{ t('nodes.noVersions') }}</v-list-item-title></v-list-item>
        </v-list>
      </v-card>
    </v-dialog>

    <v-dialog v-model="addNetworkOpen" max-width="420">
      <v-card class="pa-4">
        <v-card-title>添加网络</v-card-title>
        <v-text-field v-model="newNetworkName" label="网络名称" density="compact" variant="outlined" hide-details class="mt-2" />
        <div class="d-flex justify-end gap-2 mt-4">
          <v-btn variant="text" @click="addNetworkOpen = false">取消</v-btn>
          <v-btn color="primary" @click="createNetwork">创建</v-btn>
        </div>
      </v-card>
    </v-dialog>

    <v-dialog v-model="addNodeOpen" max-width="480">
      <v-card>
        <v-card-title>添加节点</v-card-title>
        <v-card-text>
          <v-text-field v-model="newNodeId" density="compact" variant="outlined" label="节点 ID" placeholder="例如 hk-haridc-275" class="mb-3" hint="字母、数字、- _ .，安装后不可更改" persistent-hint />
          <v-text-field v-model="newNodeAlias" density="compact" variant="outlined" label="显示名称（可选）" placeholder="留空使用主机名" />
          <v-alert v-if="addNodeError" type="error" variant="tonal" density="compact" class="mt-3">{{ addNodeError }}</v-alert>
        </v-card-text>
        <v-card-actions>
          <v-spacer />
          <v-btn variant="text" @click="addNodeOpen = false">{{ t('common.cancel') }}</v-btn>
          <v-btn color="primary" :loading="addNodeSaving" @click="createNode">创建</v-btn>
        </v-card-actions>
      </v-card>
    </v-dialog>

    <v-dialog v-model="installOpen" max-width="760">
      <v-card>
        <v-card-title>安装节点 <span class="text-medium-emphasis text-body-2">{{ installNodeId }}</span></v-card-title>
        <v-card-text>
          <v-text-field v-model="installEndpoint" density="compact" variant="outlined" label="Console 地址" class="mb-2" />
          <v-text-field v-model="installToken" density="compact" variant="outlined" label="节点 Token" class="mb-3" />
          <div class="d-flex flex-wrap gap-2 mb-3">
            <v-text-field v-model="installVersion" density="compact" variant="outlined" label="安装版本" placeholder="latest" hide-details style="max-width: 180px" />
            <v-text-field v-model="installProxy" density="compact" variant="outlined" label="GitHub 代理" placeholder="https://ghproxy.example/" hide-details style="max-width: 240px" />
            <v-text-field v-model="installDir" density="compact" variant="outlined" label="安装目录" placeholder="/opt/dokitier" hide-details style="max-width: 200px" />
            <v-text-field v-model="installService" density="compact" variant="outlined" label="服务名称" placeholder="dokitier-core" hide-details style="max-width: 200px" />
            <v-text-field v-model="installInterval" density="compact" variant="outlined" label="采集间隔(秒)" placeholder="1" hide-details style="max-width: 140px" />
            <v-checkbox v-model="installInsecure" density="compact" label="忽略证书" hide-details style="max-width: 130px" />
          </div>
          <div class="install-command">
            <pre>{{ installCommand }}</pre>
            <v-btn size="small" variant="tonal" prepend-icon="mdi-content-copy" @click="copyInstallCommand">{{ copied ? '已复制' : '复制命令' }}</v-btn>
          </div>
          <v-alert v-if="installError" type="error" variant="tonal" density="compact" class="mt-3">{{ installError }}</v-alert>
          <div class="text-caption mt-2">在目标机器上以 root 执行该命令；安装完成后节点会自动连到 console，并在列表中出现。</div>
        </v-card-text>
        <v-card-actions>
          <v-btn variant="text" @click="rotateToken" :loading="rotating">{{ '重置 Token' }}</v-btn>
          <v-spacer />
          <v-btn variant="text" @click="installOpen = false">{{ t('common.close') || '关闭' }}</v-btn>
        </v-card-actions>
      </v-card>
    </v-dialog>

    <v-dialog v-model="aliasOpen" max-width="420">
      <v-card class="pa-4">
        <v-card-title>{{ t('nodes.setAlias') }}</v-card-title>
        <v-text-field v-model="aliasValue" density="compact" variant="outlined" hide-details class="mt-2" :label="t('nodes.columns.name')" />
        <div class="d-flex justify-end gap-2 mt-4">
          <v-btn variant="text" @click="aliasOpen = false">{{ t('common.cancel') }}</v-btn>
          <v-btn color="primary" :loading="savingAlias" @click="saveAlias">{{ t('common.save') }}</v-btn>
        </div>
      </v-card>
    </v-dialog>

    <v-snackbar v-model="toast.show" :color="toast.color" :timeout="toast.timeout">
      <div class="d-flex align-center gap-2">
        <v-progress-circular v-if="toast.loading" indeterminate size="16" width="2" />
        <span>{{ toast.text }}</span>
      </div>
    </v-snackbar>
  </div>
  </template>

<script setup>
import { computed, markRaw, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, triggerRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { api } from '@/api'
import DetailDrawer from '@/components/DetailDrawer.vue'
import DokTable from '@/components/DokTable.vue'
import NetworkConfigEditor from '@/components/NetworkConfigEditor.vue'
import {
  protocolClass,
  protocolLabel,
  protocolRaw,
} from '@/utils/protocol'

const { t } = useI18n()
const headers = computed(() => [
  { title: t('nodes.columns.status'), key: 'status', width: 88 },
  { title: t('nodes.columns.name'), key: 'alias', width: 220 },
  { title: t('nodes.columns.vpnIp'), key: 'vpnIp', width: 132 },
  { title: t('nodes.columns.publicIp'), key: 'publicIp', width: 180 },
  { title: t('nodes.columns.version'), key: 'version', width: 190 },
  { title: t('nodes.columns.cpu'), key: 'cpu', width: 160 },
  { title: t('nodes.columns.mem'), key: 'mem', width: 160 },
  { title: t('nodes.columns.network'), key: 'traffic', sortable: false, width: 200 },
  { title: '流量', key: 'trafficTotal', sortable: false, width: 110 },
  { title: t('nodes.columns.actions'), key: 'actions', sortable: false },
])

const nodes = shallowRef([])
const nodeRows = new Map()
const loading = ref(false)
const drawer = ref(false)
const detail = ref(null)
const network = ref('')
const networkFilter = ref('')
const configText = ref('')
const configLoading = ref(false)
const configError = ref('')
const saving = ref(false)
const saved = ref(false)
const versionsOpen = ref(false)
const versions = ref([])
const rollingBack = ref(null)
const addNodeOpen = ref(false)
const newNodeId = ref('')
const newNodeAlias = ref('')
const addNodeError = ref('')
const addNodeSaving = ref(false)
const installOpen = ref(false)
const installNodeId = ref('')
const installToken = ref('')
const installError = ref('')
const rotating = ref(false)
const copied = ref(false)
const installEndpoint = ref(defaultInstallEndpoint())
const installVersion = ref('')
const installProxy = ref('')
const installDir = ref('')
const installService = ref('')
const installInterval = ref('')
const installInsecure = ref(false)
const aliasOpen = ref(false)
const aliasValue = ref('')
const aliasTarget = ref(null)
const savingAlias = ref(false)
const configEditor = ref(null)
const draftNetwork = ref('')
const drawerTab = ref('network')
const nodeAlias = ref('')
const nodeBandwidthUp = ref('')
const nodeBandwidthDown = ref('')
const nodeIntranetCap = ref('')
const nodeCountry = ref('')
const nodeLat = ref('')
const nodeLon = ref('')
const nodeInternetCap = ref('')
const nodeSharedListeners = ref([])
const sharedProtoOptions = ['tcp', 'udp', 'quic', 'ws', 'wss']
const savingNodeSettings = ref(false)
const nodeSaveError = ref('')
const toast = ref({ show: false, text: '', color: 'success', loading: false, timeout: 3000 })

function showToast(text, color = 'success', { loading = false, timeout = 3000 } = {}) {
  const next = { show: true, text: String(text ?? ''), color, loading, timeout }
  if (toast.value.show) {
    // 强制关闭后重开，确保 timeout/loading 状态对已有 snackbar 也生效
    toast.value = { ...toast.value, show: false }
    nextTick(() => { toast.value = next })
  } else {
    toast.value = next
  }
}

function finishToast(text, color, timeout = 3000) {
  showToast(text, color, { loading: false, timeout })
}
const sysInfo = computed(() => detail.value?.system_info || detail.value?.snapshot?.system || {})
const sysSnapshot = computed(() => detail.value?.snapshot || {})
const sysNetwork = computed(() => sysInfo.value?.network || {})
const ipv4List = computed(() => (sysNetwork.value.interfaces || []).flatMap((nic) => nic.ipv4 || []).join('  '))
const ipv6List = computed(() => (sysNetwork.value.interfaces || []).flatMap((nic) => nic.ipv6 || []).join('  '))
const tunnelRows = computed(() => {
  const rows = []
  for (const inst of sysSnapshot.value.instances || []) {
    const network = inst.network_name || '-'
    for (const edge of (inst.topology || {}).edges || []) {
      for (const t of edge.tunnels || []) {
        rows.push({
          network,
          type: t.type || '',
          typeLabel: protocolLabel(t.type),
          typeClass: protocolClass(t.type),
          typeTitle: protocolRaw(t.type),
          remote: t.remote_addr || `${edge.a} / ${edge.b}`,
          latency: t.latency_ms,
          loss: t.loss_pct,
          transportLoss: t.transport_loss_pct,
          capacity: t.capacity_mbps,
          capacitySource: t.capacity_source,
          utilization: t.utilization,
        })
      }
    }
  }
  return rows
})
const uptimeText = computed(() => {
  const boot = Number(sysInfo.value.boot_time_secs || 0)
  if (!boot) return '-'
  return fmtUptime(Date.now() / 1000 - boot)
})
const addNetworkOpen = ref(false)
const newNetworkName = ref('')

function openAlias(item) {
  aliasTarget.value = item
  aliasValue.value = item.alias || ''
  aliasOpen.value = true
}

function defaultInstallEndpoint() {
  if (typeof window === 'undefined') return 'wss://your-console/api/agent/ws'
  const origin = window.location.origin
  if (origin.startsWith('https://')) return 'wss://' + origin.slice(8) + '/api/agent/ws'
  if (origin.startsWith('http://')) return 'ws://' + origin.slice(7) + '/api/agent/ws'
  return 'wss://your-console/api/agent/ws'
}

function openAddNode() {
  newNodeId.value = ''
  newNodeAlias.value = ''
  addNodeError.value = ''
  addNodeOpen.value = true
}

async function createNode() {
  addNodeSaving.value = true
  addNodeError.value = ''
  try {
    const res = await api.createNode({ node_id: newNodeId.value.trim(), alias: newNodeAlias.value.trim() || null })
    if (!res.ok) throw new Error(res.error || 'create failed')
    addNodeOpen.value = false
    installNodeId.value = res.node_id
    installToken.value = res.token
    installError.value = ''
    copied.value = false
    installOpen.value = true
    await load()
  } catch (e) {
    addNodeError.value = String(e.message || e)
  } finally {
    addNodeSaving.value = false
  }
}

async function openInstall(item) {
  installNodeId.value = item.node_id
  installToken.value = ''
  installError.value = ''
  copied.value = false
  installOpen.value = true
  try {
    const res = await api.nodeToken(item.node_id)
    if (res.ok) installToken.value = res.token
    else installError.value = res.error || '无法获取 token'
  } catch (e) {
    installError.value = String(e.message || e)
  }
}

async function rotateToken() {
  rotating.value = true
  try {
    const res = await api.rotateNodeToken(installNodeId.value)
    if (res.ok) installToken.value = res.token
    else installError.value = res.error || '重置失败'
  } finally {
    rotating.value = false
  }
}

const installCommand = computed(() => {
  const parts = [
    'wget -qO- https://raw.githubusercontent.com/adokiu/dokitier/main/scripts/install.sh | sudo bash -s --',
    '-e', shellQuote(installEndpoint.value),
    '-t', shellQuote(installToken.value || '<token>'),
    '-n', shellQuote(installNodeId.value),
  ]
  if (installVersion.value.trim()) parts.push('--version', shellQuote(installVersion.value.trim()))
  if (installProxy.value.trim()) parts.push('--github-proxy', shellQuote(installProxy.value.trim().replace(/\/+$/, '')))
  if (installDir.value.trim()) parts.push('--install-dir', shellQuote(installDir.value.trim()))
  if (installService.value.trim()) parts.push('--service-name', shellQuote(installService.value.trim()))
  if (installInterval.value.trim()) parts.push('-i', shellQuote(installInterval.value.trim()))
  if (installInsecure.value) parts.push('--insecure')
  return parts.join(' ')
})

function shellQuote(text) {
  const value = String(text ?? '')
  return /^[A-Za-z0-9_./:@-]+$/.test(value) ? value : `'${value.replace(/'/g, `'\\''`)}'`
}

async function copyInstallCommand() {
  try {
    await navigator.clipboard.writeText(installCommand.value)
    copied.value = true
    setTimeout(() => { copied.value = false }, 1500)
  } catch {}
}

async function saveAlias() {
  if (!aliasTarget.value) return
  savingAlias.value = true
  try {
    await api.setAlias(aliasTarget.value.node_id, aliasValue.value)
    aliasTarget.value.alias = aliasValue.value
    aliasOpen.value = false
    finishToast('节点名称已保存', 'success', 3000)
  } catch (e) {
    finishToast(String(e.message || e), 'error', 5000)
  } finally {
    savingAlias.value = false
  }
}

function joinEndpoint(proto, addr) {
  const value = String(addr || '').trim()
  if (!value) return ''
  if (value.includes('://')) return value
  return `${proto || 'tcp'}://${value}`
}

function splitEndpoint(url) {
  const value = String(url || '').trim()
  const match = value.match(/^([a-z0-9+.-]+):\/\/(.*)$/i)
  if (match) return { proto: match[1].toLowerCase(), addr: match[2] }
  return { proto: 'tcp', addr: value }
}

function capacitySourceLabel(source) {
  return { probe: '实测', unmeasured: '未测量' }[source] || source || '-'
}

function fmtUptime(seconds) {
  const total = Number(seconds || 0)
  if (!total) return '-'
  const d = Math.floor(total / 86400)
  const h = Math.floor((total % 86400) / 3600)
  const m = Math.floor((total % 3600) / 60)
  return `${d}d ${h}h ${m}m`
}

const TRAFFIC_UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB', 'EB']

function humanBytes(value) {
  let amount = Number(value || 0)
  let unit = 0
  while (amount >= 1024 && unit < TRAFFIC_UNITS.length - 1) {
    amount /= 1024
    unit += 1
  }
  return `${amount.toFixed(1)} ${TRAFFIC_UNITS[unit]}`
}

function trafficTotal(item) {
  const map = item.traffic || {}
  if (networkFilter.value && map[networkFilter.value] != null) return map[networkFilter.value]
  return Object.values(map).reduce((sum, value) => sum + Number(value || 0), 0)
}

function fmtBytes(v) {
  if (v == null) return '-'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let n = Number(v)
  let i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  return `${n.toFixed(i >= 3 ? 1 : 0)} ${units[i]}`
}

function memText(item) {
  if (!item.mem_total) return '-'
  const pct = item.mem_pct != null ? item.mem_pct.toFixed(0) : Math.round(((item.mem_used || 0) / item.mem_total) * 100)
  return `${fmtBytes(item.mem_used)} / ${fmtBytes(item.mem_total)} (${pct}%)`
}

let events = null
let retry = null

// Merge the 1s payload into stable row objects instead of rebuilding the whole
// reactive array: the table re-renders from triggerRef, but no deep proxies or
// clones are created for the (large) snapshot/system-info objects.
function mergeNodes(list) {
  const next = []
  const seen = new Set()
  for (const n of list || []) {
    const id = n.node_id
    if (!id) continue
    seen.add(id)
    let row = nodeRows.get(id)
    if (!row) {
      row = {}
      nodeRows.set(id, row)
    }
    for (const key of Object.keys(n)) {
      if (key === 'snapshot' || key === 'system_info') {
        row[key] = n[key] == null ? n[key] : markRaw(n[key])
      } else {
        row[key] = n[key]
      }
    }
    row.hostname = n.hostname || row.hostname || ''
    next.push(row)
  }
  for (const id of [...nodeRows.keys()]) {
    if (!seen.has(id)) nodeRows.delete(id)
  }
  nodes.value = next
  triggerRef(nodes)
}

function connectEvents() {
  if (typeof EventSource === 'undefined') return
  events = new EventSource('/api/board/events')
  events.addEventListener('nodes', (ev) => {
    try {
      const data = JSON.parse(ev.data)
      if (Array.isArray(data.nodes)) mergeNodes(data.nodes)
    } catch {}
  })
  events.onerror = () => {
    events?.close()
    events = null
    if (retry) clearTimeout(retry)
    retry = setTimeout(connectEvents, 5000)
  }
}

async function saveNodeSettings() {
  if (!detail.value) return
  savingNodeSettings.value = true
  nodeSaveError.value = ''
  const up = nodeBandwidthUp.value === '' ? null : Number(nodeBandwidthUp.value)
  const down = nodeBandwidthDown.value === '' ? null : Number(nodeBandwidthDown.value)
  const intranet = nodeIntranetCap.value === '' ? null : Number(nodeIntranetCap.value)
  const internet = nodeInternetCap.value === '' ? null : Number(nodeInternetCap.value)
  const listeners = nodeSharedListeners.value
    .map((l) => joinEndpoint(l.proto, l.addr))
    .filter(Boolean)
    .join(',')
  try {
    showToast('正在保存并下发配置…', 'info', { loading: true, timeout: -1 })
    const res = await api.setNodeSettings(detail.value.node_id, {
      alias: nodeAlias.value,
      country_code: nodeCountry.value,
      latitude: nodeLat.value === '' ? '' : String(nodeLat.value),
      longitude: nodeLon.value === '' ? '' : String(nodeLon.value),
      bandwidth_up_mbps: up,
      bandwidth_down_mbps: down,
      bandwidth_intranet_down_mbps: intranet,
      bandwidth_internet_down_mbps: internet,
      shared_listeners: listeners,
    })
    if (!res.ok) throw new Error(res.error || '保存失败')
    detail.value.alias = nodeAlias.value
    detail.value.country_code = nodeCountry.value
    detail.value.latitude = nodeLat.value === '' ? detail.value.latitude : Number(nodeLat.value)
    detail.value.longitude = nodeLon.value === '' ? detail.value.longitude : Number(nodeLon.value)
    detail.value.bandwidth_up_mbps = up
    detail.value.bandwidth_down_mbps = down
    detail.value.bandwidth_intranet_down_mbps = intranet
    detail.value.bandwidth_internet_down_mbps = internet
    const row = nodes.value.find((n) => n.node_id === detail.value.node_id)
    if (row) {
      row.alias = nodeAlias.value
      row.bandwidth_up_mbps = up
      row.bandwidth_down_mbps = down
      row.bandwidth_intranet_down_mbps = intranet
      row.bandwidth_internet_down_mbps = internet
    }
    if (res.warning) finishToast(String(res.warning), 'warning', 5000)
    else finishToast('已保存，core 已应用配置', 'success', 3000)
  } catch (e) {
    nodeSaveError.value = String(e.message || e)
    finishToast(String(e.message || e), 'error', 5000)
  } finally {
    savingNodeSettings.value = false
  }
}

function networksOf(item) {
  return item.snapshot?.instances || item.instances || []
}

const networkOptions = computed(() => {
  const names = new Set()
  for (const node of nodes.value) for (const n of networksOf(node)) if (n.network_name) names.add(n.network_name)
  return [...names]
})

const networkFilterItems = computed(() => [
  { title: '全部网络', value: '' },
  ...networkOptions.value.map((name) => ({ title: name, value: name })),
])

const filteredNodes = computed(() => {
  if (!networkFilter.value) return nodes.value
  return nodes.value.filter((node) => networksOf(node).some((n) => n.network_name === networkFilter.value))
})

const currentNetworks = computed(() => (detail.value ? networksOf(detail.value) : []))

const networkSelectItems = computed(() => {
  const items = currentNetworks.value.map((n) => ({
    title: n.network_name,
    value: n.network_name,
    subtitle: `p2p ${n.p2p_group || '-'} / ${n.ipv4 || '-'} / ${(detail.value?.instance_ids || []).find((x) => x.network === n.network_name)?.instance_id?.slice(0, 8) || '-'}`,
  }))
  if (network.value && !items.some((i) => i.value === network.value)) {
    items.push({ title: network.value, value: network.value, subtitle: '未应用的草稿' })
  }
  return items
})

function pctText(v) {
  return `${Number(v || 0).toFixed(2)}%`
}

function usageColor(v) {
  const n = Number(v || 0)
  if (n >= 85) return '#e53935'
  if (n >= 60) return '#fb8c00'
  return '#43a047'
}

function barWidth(v) {
  return `${Math.max(0, Math.min(100, Number(v || 0)))}%`
}

function memPct(item) {
  if (item.mem_pct != null) return item.mem_pct
  if (item.mem_total) return ((item.mem_used || 0) / item.mem_total) * 100
  return 0
}

function bps(v) {
  if (v == null) return '0 B/s'
  const units = ['B/s', 'KB/s', 'MB/s', 'GB/s']
  let n = Number(v)
  let i = 0
  while (n >= 1024 && i < units.length - 1) { n /= 1024; i++ }
  const text = n >= 100 ? n.toFixed(0) : n >= 10 ? n.toFixed(1) : n.toFixed(2)
  return `${text} ${units[i]}`
}

function overlayIp(item) {
  const inst = networksOf(item)[0]
  return inst?.ipv4 || ''
}

function shortIp(ip) {
  if (!ip) return ''
  const text = String(ip)
  if (!text.includes(':') || text.length <= 24) return text
  return `${text.slice(0, 12)}…${text.slice(-8)}`
}

function ipInfo(item, kind) {
  const inst = networksOf(item)[0]
  const v4 = kind === 'vpn' ? inst?.ipv4 : item.public_ipv4
  const v6 = kind === 'vpn' ? inst?.ipv6 : item.public_ipv6
  return { v4: v4 || '', v6: shortIp(v6), v6full: v6 || '' }
}

function flagUrl(item) {
  const code = (item.country_code || '').toLowerCase()
  const safe = code.length === 2 ? code : 'un'
  return `https://flagcdn.com/${safe}.svg`
}

async function load() {
  loading.value = true
  try {
    const data = await api.boardNodes()
    mergeNodes(data.nodes)
  } finally {
    loading.value = false
  }
}

async function refreshItem(item) {
  try {
    const fresh = await api.boardNode(item.node_id)
    Object.assign(item, fresh)
  } catch {}
}

async function open(item) {
  detail.value = item
  // the realtime list omits snapshot/system_info; load the full record here
  void (async () => {
    try {
      const fresh = await api.boardNode(item.node_id)
      if (fresh && !fresh.error) {
        Object.assign(item, fresh)
        item.snapshot = fresh.snapshot == null ? fresh.snapshot : markRaw(fresh.snapshot)
        item.system_info = fresh.system_info == null ? fresh.system_info : markRaw(fresh.system_info)
        triggerRef(nodes)
      }
    } catch {}
  })()
  nodeAlias.value = item.alias || ''
  nodeBandwidthUp.value = item.bandwidth_up_mbps ?? item.bandwidth_mbps ?? ''
  nodeBandwidthDown.value = item.bandwidth_down_mbps ?? item.bandwidth_mbps ?? ''
  nodeIntranetCap.value = item.bandwidth_intranet_down_mbps ?? ''
  nodeCountry.value = item.country_code || ''
  nodeLat.value = item.latitude == null ? '' : String(item.latitude)
  nodeLon.value = item.longitude == null ? '' : String(item.longitude)
  nodeSharedListeners.value = String(item.shared_listeners || '')
    .split(',')
    .map((url) => url.trim())
    .filter(Boolean)
    .map(splitEndpoint)
  nodeInternetCap.value = item.bandwidth_internet_down_mbps ?? ''
  drawerTab.value = 'network'
  drawer.value = true
  network.value = networksOf(item)[0]?.network_name || ''
  configText.value = ''
  configError.value = ''
  saved.value = false
  if (network.value) await loadConfig()
}

async function deleteNetwork(name) {
  if (!window.confirm(`删除网络 ${name}？该操作会停止并移除该网络的本地配置`)) return
  try {
    const res = await api.boardCommand(detail.value.node_id, { kind: 'delete_network', network: name }, name)
    if (!res.ok) throw new Error(res.error || 'delete failed')
    if (network.value === name) {
      network.value = ''
      configText.value = ''
    }
    await load()
  } catch (e) {
    configError.value = String(e.message || e)
  }
}

async function selectNetwork(name) {
  draftNetwork.value = ''
  network.value = name
  configText.value = ''
  configError.value = ''
  saved.value = false
  await loadConfig()
}

function networkTemplate(name) {
  return [
    'hostname = ""',
    'ipv4 = ""',
    '',
    '[network_identity]',
    `network_name = "${name}"`,
    'network_secret = ""',
    '',
    '[flags]',
  ].join('\n')
}

function createNetwork() {
  const name = newNetworkName.value.trim()
  if (!name) return
  network.value = name
  draftNetwork.value = name
  configText.value = networkTemplate(name)
  configError.value = ''
  saved.value = false
  addNetworkOpen.value = false
  newNetworkName.value = ''
}

async function loadConfig() {
  configLoading.value = true
  configError.value = ''
  saved.value = false
  try {
    const res = await api.boardVersions(detail.value.node_id)
    const latest = (res.versions || []).find((v) => v.network === network.value)
    if (!latest) {
      configText.value = networkTemplate(network.value)
      return
    }
    const full = await api.boardVersion(detail.value.node_id, latest.id)
    configText.value = full.toml || networkTemplate(network.value)
  } catch (e) {
    configError.value = String(e.message || e)
  } finally {
    configLoading.value = false
  }
}

async function saveConfig() {
  const problem = configEditor.value?.validate?.()
  if (problem) {
    configError.value = problem
    return
  }
  saving.value = true
  configError.value = ''
  saved.value = false
  try {
    showToast('正在下发网络配置…', 'info', { loading: true, timeout: -1 })
    const precheck = await api.precheckConfig(network.value, configText.value)
    if (!precheck.ok) throw new Error((precheck.errors || []).join('; ') || 'precheck failed')
    const res = await api.boardCommand(detail.value.node_id, { kind: 'apply_network', network: network.value, toml: configText.value }, network.value)
    if (!res.ok) throw new Error(res.error || 'apply failed')
    saved.value = true
    draftNetwork.value = ''
    await load()
    finishToast('网络配置已下发，core 已应用', 'success', 3000)
  } catch (e) {
    configError.value = String(e.message || e)
    finishToast(String(e.message || e), 'error', 5000)
  } finally {
    saving.value = false
  }
}

async function openVersions() {
  versionsOpen.value = true
  try {
    const data = await api.boardVersions(detail.value.node_id)
    versions.value = data.versions || []
  } catch {
    versions.value = []
  }
}

async function rollback(version) {
  rollingBack.value = version.id
  try {
    const data = await api.boardVersion(detail.value.node_id, version.id)
    const target = data.network || network.value
    const res = await api.boardCommand(detail.value.node_id, { kind: 'apply_network', network: target, toml: data.toml }, target)
    if (!res.ok) throw new Error(res.error || 'rollback failed')
    versionsOpen.value = false
    saved.value = true
    configText.value = data.toml
    await load()
  } catch (e) {
    configError.value = String(e.message || e)
  } finally {
    rollingBack.value = null
  }
}

function downloadConfig() {
  const blob = new Blob([configText.value], { type: 'text/plain' })
  const a = document.createElement('a')
  a.href = URL.createObjectURL(blob)
  a.download = `${detail.value?.node_id || 'node'}-${network.value}.toml`
  a.click()
  URL.revokeObjectURL(a.href)
}

onMounted(() => {
  load()
  connectEvents()
})

onBeforeUnmount(() => {
  if (events) events.close()
  if (retry) clearTimeout(retry)
})
</script>

<style scoped>

.page-sub { margin-top: 4px; font-size: 12px; color: var(--dok-muted); }
.node-name { min-width: 0; gap: 8px !important; }
.node-alias { max-width: 170px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ip-cell { font-size: inherit; line-height: 1.15; }
:deep(.v-data-table__td:nth-child(6)),
:deep(.v-data-table__td:nth-child(7)),
:deep(.v-data-table__th:nth-child(6)),
:deep(.v-data-table__th:nth-child(7)) { padding-inline: 0 !important; }
.nodes-table :deep(.v-table__wrapper) { overflow-x: auto !important; overflow-y: hidden !important; }
.nodes-table :deep(table) { width: 100% !important; min-width: 1500px; table-layout: fixed; }
.nodes-table :deep(th:nth-child(6)), .nodes-table :deep(td:nth-child(6)),
.nodes-table :deep(th:nth-child(7)), .nodes-table :deep(td:nth-child(7)) { width: 160px; max-width: 160px; overflow: hidden; }
.nodes-table :deep(td) { overflow: hidden; text-overflow: ellipsis; }
.nodes-table :deep(.traffic),
.nodes-table :deep(.traffic-total),
.nodes-table :deep(.meter-value),
.nodes-table :deep(.ip-cell) { font-variant-numeric: tabular-nums; }
.nodes-table :deep(th), .nodes-table :deep(td) { white-space: nowrap; }
.nodes-table :deep(.v-data-table__td) { vertical-align: middle; }
.meter-cell { display: block; width: 120px; overflow: hidden; }
.meter-value { line-height: 16px; font-size: 12px; }
.meter-track { display: block; width: 120px; height: 4px; margin-top: 3px; border-radius: 999px; background: rgba(var(--v-theme-on-surface), .14); overflow: hidden; }
.meter-value { white-space: nowrap; }
.meter-fill { height: 100%; border-radius: 999px; transition: width .3s ease, background .3s ease; }
.version-cell { white-space: nowrap; }
.info-icon { color: rgba(var(--v-theme-on-surface), .45); cursor: help; }
.install-command pre { white-space: pre-wrap; word-break: break-all; font-size: 12px; line-height: 1.5; background: rgba(var(--v-theme-on-surface), .05); border: 1px solid rgba(var(--v-theme-on-surface), .12); border-radius: 8px; padding: 10px; margin: 0 0 8px; }
.ip-v6 { color: var(--dok-subtle); }
.node-flag { width: 24px; height: 16px; border-radius: 2px; object-fit: cover; flex: 0 0 auto; }
.node-host { font-weight: 500; }
.traffic { display: flex; gap: 12px; font-size: inherit; color: var(--dok-muted); }
.traffic .up { color: #2e7d32; }
.traffic .down { color: #1565c0; }
.network-picker { max-width: 280px; }
.config-actions {
  margin-top: 18px;
  padding-top: 14px;
  border-top: 1px dashed rgba(var(--v-theme-on-surface), .14);
}
.net-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 12px; border-radius: 10px; cursor: pointer; }
.net-row:hover { background: rgba(60, 150, 202, .05); }
.net-row.active { background: rgba(60, 150, 202, .08); }
.net-name { font-size: 13px; font-weight: 600; }
.net-meta { margin-top: 3px; font-size: 11px; color: var(--dok-muted); }
.config-editor :deep(textarea) { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.55; }

.drawer-tabs { border-bottom: 1px solid var(--dok-table-line); }
.node-settings { padding-top: 4px; }
.shared-listener-row { display: grid; grid-template-columns: 90px minmax(0, 1fr) 36px; gap: 8px; align-items: center; margin-bottom: 8px; }
.shared-listener-row :deep(.v-input) { width: 100%; }
.node-field-hint { margin: -4px 0 8px; font-size: 12px; color: rgba(var(--v-theme-on-surface), .58); }
.drawer-panels { overflow: visible; }
.drawer-panels > div { min-width: 0; }
.drawer-tabs :deep(.v-tab) { font-size: 13px; text-transform: none; letter-spacing: 0; }
.sys-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.cap-row { display: flex; flex-wrap: wrap; gap: 6px; }
.sys-card {
  padding: 14px;
  border: 1px solid rgba(var(--v-theme-on-surface), .12);
  border-radius: 10px;
  background: rgba(var(--v-theme-on-surface), .02);
}
.sys-card--wide { grid-column: 1 / -1; }
.sys-title { font-size: 11px; font-weight: 600; letter-spacing: .05em; text-transform: uppercase; color: rgba(var(--v-theme-on-surface), .55); margin-bottom: 8px; }
.sys-main { font-size: 16px; font-weight: 700; line-height: 1.35; overflow-wrap: anywhere; }
.sys-sub { margin-top: 6px; font-size: 12px; color: rgba(var(--v-theme-on-surface), .66); }
.sys-bar { margin-top: 10px; height: 6px; border-radius: 999px; background: rgba(var(--v-theme-on-surface), .12); overflow: hidden; }
.sys-bar-fill { height: 100%; border-radius: 999px; background: rgb(var(--v-theme-primary)); }
.kv { display: flex; justify-content: space-between; gap: 16px; padding: 7px 0; font-size: 13px; border-top: 1px dashed rgba(var(--v-theme-on-surface), .1); }
.kv:first-of-type { border-top: 0; }
.kv span { color: rgba(var(--v-theme-on-surface), .6); white-space: nowrap; }
.kv b { font-weight: 500; text-align: right; overflow-wrap: anywhere; }

.nic-scroll { overflow-x: auto; }
.nic-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.nic-table th, .nic-table td { padding: 7px 10px; text-align: left; white-space: nowrap; border-top: 1px solid rgba(var(--v-theme-on-surface), .1); }
.nic-table th { color: rgba(var(--v-theme-on-surface), .6); font-weight: 500; border-top: 0; }
.nic-table td { max-width: 220px; overflow: hidden; text-overflow: ellipsis; }
</style>
