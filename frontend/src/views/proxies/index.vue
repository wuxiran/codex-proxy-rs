<script setup lang="ts">
import type { OutboundProxyRecord } from '@/api'
import { ZButton, ZCard, ZCheckbox, ZConfirmDialog, ZIconButton, ZInput, ZPageHeader, ZPagination, ZTable, ZTag } from '@codex-proxy/ui'
import { LockKeyhole, Plus, Search, ShieldCheck, Trash2, Users, Wifi, X } from '@lucide/vue'
import { shallowRef, watch } from 'vue'
import { usePageSelection } from '@/composables/usePageSelection'
import ProxyAccountsModal from './components/ProxyAccountsModal.vue'
import ProxyActions from './components/ProxyActions.vue'
import ProxyEndpointCopy from './components/ProxyEndpointCopy.fork.vue' // fork: proxy-copy
import ProxyFormModal from './components/ProxyFormModal.vue'
import ProxyQualityCell from './components/ProxyQualityCell.fork.vue' // fork: proxy-quality
import ProxyQualityReportModal from './components/ProxyQualityReportModal.vue' // fork: proxy-quality
import ProxyRegion from './components/ProxyRegion.vue'
import { useProxies } from './composables/useProxies'
import { useProxyBatchDelete } from './composables/useProxyBatchDelete.fork' // fork: proxy-batch-delete
import { useProxyBatchTesting } from './composables/useProxyBatchTesting'
import { useProxyQuality } from './composables/useProxyQuality.fork' // fork: proxy-quality
import { proxyColumns } from './constants'
import { exitGeoLabel } from './presenter.fork' // fork: proxy-quality

const {
  search,
  proxies,
  loading,
  pagination,
  loadProxies,
  reloadAfterCreate,
  showForm,
  editing,
  form,
  formTestResult,
  saving,
  showDelete,
  pendingDelete,
  deleting,
  testingIds,
  updatingIds,
  showDisable,
  pendingDisable,
  disabling,
  toggleEnabled,
  confirmDisable,
  testingConnection,
  detectingLocation,
  openForm,
  checkProxy,
  testSavedProxies,
  testConnection,
  detectLocation,
  save,
  requestDelete,
  confirmDelete,
  setPage,
  setPageSize,
  clearCredentials,
} = useProxies()
const showAccounts = shallowRef(false)
const inspected = shallowRef<OutboundProxyRecord | null>(null)
const { selectedIds, selectedRowKeys, allSelected, indeterminate, toggleSelection, toggleAll } = usePageSelection(proxies)
const { busy: batchTesting, start: testSelectedProxies } = useProxyBatchTesting({
  test: testSavedProxies,
  reload: () => loadProxies({ silent: true }),
})
// fork: proxy-quality
const quality = useProxyQuality({ proxies, reload: () => loadProxies({ silent: true }) })
const { checkingIds: qualityCheckingIds, batchRunning: batchQualityRunning, showReport, reportProxy, freshReport } = quality
// fork: proxy-batch-delete
const { showBatchDelete, batchDeleting, confirmBatchDelete } = useProxyBatchDelete({ proxies, selectedIds, reload: () => loadProxies() })

watch(search, () => {
  selectedIds.value = new Set()
})
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <ZPageHeader
      class="h-17"
      title="代理管理"
      description="管理账号使用的代理，测试连接、出口地区与上游可达性"
    />
    <ZCard class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col">
      <template #header>
        <div class="flex w-full flex-col gap-3 sm:flex-row sm:items-center">
          <ZInput v-model="search" :disabled="batchTesting || batchQualityRunning" class="sm:w-80" aria-label="搜索代理" placeholder="搜索代理名称...">
            <template #prefix>
              <Search class="size-4.5 text-cp-text-tertiary" />
            </template>
          </ZInput>
          <div class="flex shrink-0 flex-wrap items-center justify-end gap-2 sm:ml-auto">
            <ZButton v-if="selectedIds.size" :loading="batchTesting" :disabled="loading || batchQualityRunning" aria-label="批量测试代理" @click="testSelectedProxies(selectedRowKeys)">
              <Wifi class="size-4" />
              批量测试 ({{ selectedIds.size }})
            </ZButton>
            <!-- fork: proxy-quality — 批量质量检测 -->
            <template v-if="batchQualityRunning">
              <span class="text-cp-sm tabular-nums text-cp-text-secondary" aria-live="polite">
                质量检测中 {{ quality.batchProgressLabel.value }}
              </span>
              <ZButton aria-label="取消批量质量检测" @click="quality.cancelBatch()">
                <X class="size-4" />
                取消
              </ZButton>
            </template>
            <ZButton v-else-if="selectedIds.size" :disabled="loading || batchTesting" aria-label="批量质量检测" @click="quality.batchQuality(selectedRowKeys)">
              <ShieldCheck class="size-4" />
              批量质量检测 ({{ selectedIds.size }})
            </ZButton>
            <!-- fork: proxy-batch-delete -->
            <ZButton v-if="selectedIds.size" type="danger" variant="plain" :disabled="loading || batchTesting || batchQualityRunning" aria-label="批量删除代理" @click="showBatchDelete = true">
              <Trash2 class="size-4" />
              批量删除 ({{ selectedIds.size }})
            </ZButton>
            <ZButton type="primary" @click="openForm()">
              <template #icon>
                <Plus class="size-4" />
              </template>
              新增代理
            </ZButton>
          </div>
        </div>
      </template>
      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <ZTable class="min-h-0 flex-1" :columns="proxyColumns" :data="proxies" :loading="loading" :selected-row-keys="selectedRowKeys" :empty-text="search.trim() ? '没有找到匹配的代理，请尝试其他名称' : '暂无代理，请点击新增代理添加'">
            <template #header-selection>
              <ZCheckbox :model-value="allSelected" :indeterminate="indeterminate" :disabled="batchTesting || batchQualityRunning" aria-label="选择当前页代理" @update:model-value="toggleAll" />
            </template>
            <template #selection="{ row }">
              <ZCheckbox :model-value="selectedIds.has(row.id)" :disabled="batchTesting || batchQualityRunning" :aria-label="`选择代理 ${row.name}`" @update:model-value="toggleSelection(row.id)" />
            </template>
            <template #name="{ row }">
              <span class="block truncate text-cp text-cp-text" :title="row.name">{{ row.name }}</span>
            </template>
            <template #address="{ row }">
              <span class="flex min-w-0 items-center gap-1 text-cp-xs font-emphasis text-cp-text-quaternary">
                <LockKeyhole v-if="row.hasAuthentication" class="size-3 shrink-0" aria-label="已保存代理认证" />
                <span class="truncate font-mono" :title="row.endpoint">{{ row.endpoint }}</span>
                <ProxyEndpointCopy :endpoint="row.endpoint" :has-authentication="row.hasAuthentication" /> <!-- fork: proxy-copy -->
              </span>
            </template>
            <template #status="{ row }">
              <ZTag :type="row.enabled ? 'success' : 'default'">
                {{ row.enabled ? '已启用' : '已禁用' }}
              </ZTag>
            </template>
            <template #exitIp="{ row }">
              <div v-if="row.lastTest?.exitIpv4 && row.lastTest?.exitIpv6" class="flex flex-col gap-0.5 font-mono text-cp-xs">
                <span class="truncate" :title="`IPv4: ${row.lastTest.exitIpv4}`">
                  {{ row.lastTest.exitIpv4 }}
                </span>
                <span class="truncate" :title="`IPv6: ${row.lastTest.exitIpv6}`">
                  {{ row.lastTest.exitIpv6 }}
                </span>
              </div>
              <div v-else-if="row.lastTest?.exitIpv4" class="truncate font-mono text-cp-xs" :title="`IPv4: ${row.lastTest.exitIpv4}`">
                {{ row.lastTest.exitIpv4 }}
              </div>
              <div v-else-if="row.lastTest?.exitIpv6" class="truncate font-mono text-cp-xs" :title="`IPv6: ${row.lastTest.exitIpv6}`">
                {{ row.lastTest.exitIpv6 }}
              </div>
              <span v-else-if="row.lastTest?.exitIp" class="block truncate font-mono text-cp-xs" :title="row.lastTest.exitIp">{{ row.lastTest.exitIp }}</span>
              <span v-else class="text-cp-text-quaternary">-</span>
            </template>
            <template #region="{ row }">
              <div class="grid min-w-0 gap-0.5">
                <ProxyRegion :proxy="row" class="text-cp-sm" />
                <!-- fork: proxy-quality — 质量检测观测到的出口地理；Windows 不渲染国旗 emoji，统一用地区码标签 -->
                <span v-if="row.lastTest?.exitGeo" class="flex min-w-0 items-center gap-1.5 text-cp-xs text-cp-text-secondary" :title="[row.lastTest.exitGeo.country, row.lastTest.exitGeo.region, row.lastTest.exitGeo.city].filter(Boolean).join(' / ')">
                  <span class="shrink-0 rounded-cp-sm bg-cp-fill-secondary px-1 font-mono text-[10px] leading-4 font-bold text-cp-text-secondary">{{ row.lastTest.exitGeo.countryCode }}</span>
                  <span class="truncate">{{ exitGeoLabel(row.lastTest.exitGeo) }}</span>
                </span>
              </div>
            </template>
            <template #latency="{ row }">
              <div class="grid justify-items-start gap-1">
                <span v-if="testingIds.has(row.id)" class="text-cp-text-secondary">测试中</span>
                <span v-else-if="row.lastTest?.success" class="tabular-nums text-cp-success">
                  {{ row.lastTest.latencyMs }} ms
                </span>
                <span v-else-if="row.lastTest" class="text-cp-error" :title="`${row.lastTest.message}（耗时 ${row.lastTest.latencyMs} ms）`">失败</span>
                <span v-else class="text-cp-text-quaternary">未测试</span>
                <ProxyQualityCell :proxy="row" :checking="qualityCheckingIds.has(row.id)" @open="quality.openReport" /> <!-- fork: proxy-quality -->
              </div>
            </template>
            <template #accounts="{ row }">
              <button type="button" class="flex w-fit cursor-pointer items-center gap-1.5 rounded-sm border-0 bg-transparent p-0 text-cp-sm text-cp-text-secondary outline-none transition-colors hover:text-cp-primary-text focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container" :aria-label="`查看 ${row.name} 的 ${row.accountCount} 个关联账号`" @click="inspected = row; showAccounts = true">
                <Users class="size-3.5" aria-hidden="true" />
                <span class="font-mono tabular-nums">{{ row.accountCount }}</span>
              </button>
            </template>
            <template #testedAt="{ row }">
              {{ row.lastTestAtDisplay ?? '-' }}
            </template>
            <template #actions="{ row }">
              <div class="flex items-center gap-0.5">
                <!-- fork: proxy-quality -->
                <ZIconButton size="small" aria-label="质量检测" :loading="qualityCheckingIds.has(row.id)" :disabled="testingIds.has(row.id) || qualityCheckingIds.has(row.id) || updatingIds.has(row.id)" @click="quality.checkQuality(row)">
                  <ShieldCheck class="size-3.5 text-cp-link" />
                </ZIconButton>
                <ProxyActions :proxy="row" :testing="testingIds.has(row.id)" :batch-testing="batchTesting" :updating="updatingIds.has(row.id) || qualityCheckingIds.has(row.id)" @test="checkProxy" @edit="openForm" @toggle="toggleEnabled" @delete="requestDelete" />
              </div>
            </template>
          </ZTable>
          <ZPagination v-bind="pagination" :disabled="loading" @current-change="setPage" @size-change="setPageSize" />
        </div>
      </template>
    </ZCard>

    <ProxyFormModal
      v-model="showForm"
      v-model:name="form.name"
      v-model:proxy-url="form.proxyUrl"
      v-model:max-websocket-connections="form.maxWebsocketConnections"
      v-model:custom-location="form.customLocation"
      v-model:location="form.location"
      :test-result="formTestResult"
      :proxy="editing"
      :saving="saving"
      :testing-connection="testingConnection"
      :detecting-location="detectingLocation"
      :test-saved-proxies="testSavedProxies"
      :reload-after-create="reloadAfterCreate"
      @save="save"
      @test="testConnection"
      @detect-location="detectLocation"
      @after-leave="clearCredentials"
    />
    <ZConfirmDialog v-model="showDisable" title="禁用代理" confirm-text="禁用代理" :loading="disabling" @confirm="confirmDisable">
      <p class="m-0">
        确定禁用“{{ pendingDisable?.name }}”吗？
      </p>
      <p class="mt-3 mb-0 text-cp-text-secondary">
        关联账号的新请求将改为直连，代理配置和绑定会保留，重新启用后恢复使用
      </p>
    </ZConfirmDialog>
    <ZConfirmDialog v-model="showDelete" title="删除代理" confirm-text="删除代理" destructive :loading="deleting" @confirm="confirmDelete">
      <p class="m-0">
        确定删除“{{ pendingDelete?.name }}”吗？
      </p>
      <p class="mt-3 mb-0 text-cp-text-secondary">
        将自动解除关联账号，关联账号的新请求将改为直连
      </p>
    </ZConfirmDialog>
    <!-- fork: proxy-batch-delete -->
    <ZConfirmDialog v-model="showBatchDelete" title="批量删除代理" confirm-text="删除代理" destructive :loading="batchDeleting" @confirm="confirmBatchDelete">
      <p class="m-0">
        确定删除已选的 {{ selectedIds.size }} 条代理吗？仍有账号使用的代理会被跳过。
      </p>
    </ZConfirmDialog>
    <ProxyAccountsModal v-model="showAccounts" :proxy="inspected" @removed="loadProxies({ silent: true })" />
    <!-- fork: proxy-quality -->
    <ProxyQualityReportModal
      v-model="showReport"
      :proxy="reportProxy"
      :report="freshReport"
      :checking="reportProxy !== null && qualityCheckingIds.has(reportProxy.id)"
      @recheck="quality.checkQuality"
    />
  </div>
</template>
