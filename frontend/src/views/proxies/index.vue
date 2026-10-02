<script setup lang="ts">
import type { OutboundProxyRecord, ProxyQualityReport, ProxyQualityStatus } from '@/api'
import { Copy, ListPlus, LockKeyhole, MapPin, Pencil, Plus, Search, ShieldCheck, Trash2, Users, Wifi, X } from '@lucide/vue'
import { watchDebounced } from '@vueuse/core'
import { computed, onMounted, reactive, ref, shallowRef, watch } from 'vue'
import { batchDeleteProxies, checkProxyQuality, createProxy, deleteProxy, getProxies, probeProxy, testProxy, updateProxy } from '@/api'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseCheckbox from '@/components/base/BaseCheckbox.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseMenuItem from '@/components/base/BaseMenuItem.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseTablePagination from '@/components/base/BaseTable/BaseTablePagination.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useCopyText } from '@/composables/useCopyText'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { usePageSelection } from '@/composables/usePageSelection'
import { formatDateTime } from '@/utils/date'
import { normalizeRequestLocation, requestLocationError } from '@/utils/request-location'
import ProxyAccountsModal from './components/ProxyAccountsModal.vue'
import ProxyBatchCreateModal from './components/ProxyBatchCreateModal.vue'
import ProxyFormModal from './components/ProxyFormModal.vue'
import ProxyQualityReportModal from './components/ProxyQualityReportModal.vue'
import { useProxyBatchRunner } from './composables/useProxyBatchRunner'
import { endpointCopyFormats, exitGeoLabel, latencyToneClass, qualityStatus } from './presenter'

const search = shallowRef('')
const query = usePagedQuery({
  initialPageSize: 20,
  load: (pagination, options) => getProxies({ ...pagination, search: search.value.trim() || undefined }, options),
})
const { items: proxies, loading } = query
const pagination = computed(() => ({ currentPage: query.page.value, pageSize: query.pageSize.value, total: query.total.value }))
const columns = defineTableColumns<OutboundProxyRecord>([
  { key: 'selection', kind: 'selection', hideable: false },
  { key: 'name', label: '代理名称', kind: 'identity', size: 'lg' },
  { key: 'address', label: '代理地址', kind: 'identity', size: 'xl' },
  { key: 'exitIp', label: '出口', kind: 'custom', size: 'lg' },
  { key: 'latency', label: '耗时 / 质量', kind: 'custom', size: 'md' },
  { key: 'accounts', label: '关联账号', kind: 'custom', size: 'sm' },
  { key: 'testedAt', label: '测试时间', kind: 'datetime' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'lg', fixedWidth: true },
])
const showForm = shallowRef(false)
const editing = shallowRef<OutboundProxyRecord | null>(null)
const form = reactive({
  name: '',
  proxyUrl: '',
  customLocation: false,
  location: { country: '', region: '', city: '', timezone: '' },
})
const saveAction = useAsyncAction()
const { loading: saving } = saveAction
const deleteAction = useAsyncAction()
const { loading: deleting } = deleteAction
const showDelete = shallowRef(false)
const pendingDelete = shallowRef<OutboundProxyRecord | null>(null)
const testingIds = ref(new Set<string>())
const formTestAction = useAsyncAction()
const testingForm = computed(() => formTestAction.loading.value || (editing.value !== null && testingIds.value.has(editing.value.id)))
const showAccounts = shallowRef(false)
const inspected = shallowRef<OutboundProxyRecord | null>(null)
const { selectedIds, allSelected, indeterminate, selectedRowKeys, toggleSelection, toggleAll } = usePageSelection(proxies)
const qualityCheckingIds = ref(new Set<string>())
const showReport = shallowRef(false)
const reportProxy = shallowRef<OutboundProxyRecord | null>(null)
const freshReport = shallowRef<ProxyQualityReport | null>(null)
const showBatchCreate = shallowRef(false)
const showBatchDelete = shallowRef(false)
const batchDeleteAction = useAsyncAction()
const { loading: batchDeleting } = batchDeleteAction
const batch = useProxyBatchRunner()
const copyText = useCopyText()
const copyMenuId = shallowRef<string | null>(null)
const busyIds = computed(() => new Set([...testingIds.value, ...qualityCheckingIds.value]))
const selectedCount = computed(() => selectedIds.value.size)

function openForm(proxy: OutboundProxyRecord | null = null) {
  editing.value = proxy
  form.name = proxy?.name ?? ''
  form.proxyUrl = ''
  form.customLocation = proxy?.location != null
  form.location = proxy?.location ? { ...proxy.location } : { country: '', region: '', city: '', timezone: '' }
  showForm.value = true
}

async function checkProxy(proxy: OutboundProxyRecord) {
  if (testingIds.value.has(proxy.id))
    return
  testingIds.value.add(proxy.id)
  try {
    const result = await testProxy({ id: proxy.id, revision: proxy.revision })
    if (result.lastTest?.success)
      toast.success(`${result.name}：连接成功`)
    else
      toast.error(result.lastTest?.message ?? '代理测试失败')
  }
  catch {}
  finally {
    testingIds.value.delete(proxy.id)
    await query.execute({ silent: true })
  }
}

function openReport(proxy: OutboundProxyRecord, report: ProxyQualityReport | null = null) {
  reportProxy.value = proxy
  freshReport.value = report
  showReport.value = true
}

async function runQualityCheck(proxy: OutboundProxyRecord, silent: boolean) {
  if (qualityCheckingIds.value.has(proxy.id))
    return null
  qualityCheckingIds.value.add(proxy.id)
  try {
    return await checkProxyQuality({ id: proxy.id, revision: proxy.revision }, { silent })
  }
  finally {
    qualityCheckingIds.value.delete(proxy.id)
  }
}

async function checkQuality(proxy: OutboundProxyRecord) {
  openReport(proxy)
  try {
    const result = await runQualityCheck(proxy, false)
    if (!result)
      return
    // 弹窗可能已切到别的代理；只有仍在看这一条时才替换报告。
    if (reportProxy.value?.id === proxy.id) {
      reportProxy.value = result.record
      freshReport.value = result.report
    }
    toast.success(`${result.record.name}：评分 ${result.report.score}（${result.report.grade}）`)
  }
  // 失败提示由请求层给出；弹窗保持打开，继续展示上一次的报告（如果有）。
  catch {}
  finally {
    await query.execute({ silent: true })
  }
}

/** 批量目标：有勾选用勾选；否则取当前搜索条件下的全部代理，而不只是当前页。 */
async function batchTargets(): Promise<OutboundProxyRecord[]> {
  if (selectedIds.value.size > 0)
    return proxies.value.filter(proxy => selectedIds.value.has(proxy.id))
  const filter = search.value.trim() || undefined
  const first = await getProxies({ page: 1, pageSize: 200, search: filter })
  const items = [...first.items]
  for (let page = 2; page <= first.page.totalPages; page += 1)
    items.push(...(await getProxies({ page, pageSize: 200, search: filter })).items)
  return items
}

async function batchTest(targets?: OutboundProxyRecord[]) {
  if (batch.running.value)
    return
  try {
    const items = targets ?? await batchTargets()
    if (items.length === 0) {
      toast.warning('暂无可测试的代理')
      return
    }
    let failed = 0
    const outcome = await batch.run('test', items, 3, async (proxy) => {
      testingIds.value.add(proxy.id)
      try {
        const result = await testProxy({ id: proxy.id, revision: proxy.revision }, { silent: true })
        if (!result.lastTest?.success)
          failed += 1
      }
      catch {
        failed += 1
      }
      finally {
        testingIds.value.delete(proxy.id)
      }
    })
    const summary = `${outcome.cancelled ? '已取消，' : ''}共测试 ${outcome.completed} 条`
    if (failed > 0)
      toast.warning(`${summary}，${failed} 条失败`)
    else
      toast.success(`${summary}，全部连接成功`)
  }
  catch {}
  finally {
    await query.execute({ silent: true })
  }
}

async function batchQuality() {
  if (batch.running.value)
    return
  try {
    const items = await batchTargets()
    if (items.length === 0) {
      toast.warning('暂无可检测的代理')
      return
    }
    const counts: Record<ProxyQualityStatus | 'error', number> = { healthy: 0, warn: 0, challenge: 0, failed: 0, error: 0 }
    const outcome = await batch.run('quality', items, 2, async (proxy) => {
      try {
        const result = await runQualityCheck(proxy, true)
        if (result)
          counts[result.report.status] += 1
      }
      catch {
        counts.error += 1
      }
    })
    const abnormal = counts.failed + counts.error
    toast[counts.challenge + abnormal > 0 ? 'warning' : 'success'](
      `${outcome.cancelled ? '已取消，' : ''}共检测 ${outcome.completed} 条：优质 ${counts.healthy}，告警 ${counts.warn}，挑战 ${counts.challenge}，异常 ${abnormal}`,
    )
  }
  catch {}
  finally {
    await query.execute({ silent: true })
  }
}

async function handleBatchCreated(records: OutboundProxyRecord[]) {
  search.value = ''
  query.page.value = 1
  await query.execute()
  // 未通过连接测试的代理不能绑定账号，新建后直接测一轮。
  if (records.length > 0)
    await batchTest(records)
}

async function confirmBatchDelete() {
  if (batchDeleting.value)
    return
  const items = proxies.value
    .filter(proxy => selectedIds.value.has(proxy.id))
    .map(proxy => ({ id: proxy.id, revision: proxy.revision }))
  if (items.length === 0) {
    showBatchDelete.value = false
    return
  }
  await batchDeleteAction.run(async () => {
    const result = await batchDeleteProxies({ items })
    showBatchDelete.value = false
    selectedIds.value = new Set()
    await query.execute()
    if (result.skipped.length > 0)
      toast.warning(`已删除 ${result.deletedIds.length} 条，跳过 ${result.skipped.length} 条（仍有账号使用或已被修改）`)
    else
      toast.success(`已删除 ${result.deletedIds.length} 条代理`)
  })
}

async function copyEndpoint(value: string) {
  copyMenuId.value = null
  await copyText(value, { successText: '代理地址已复制' })
}

async function testConnection() {
  if (saving.value || testingForm.value)
    return
  const proxyUrl = form.proxyUrl.trim()
  if (!proxyUrl && editing.value) {
    await checkProxy(editing.value)
    return
  }
  if (!proxyUrl) {
    toast.warning('请填写代理连接地址')
    return
  }
  await formTestAction.run(async () => {
    // 新地址只做探测，保存前不修改代理及关联账号的连接配置。
    const result = await probeProxy({ proxyUrl })
    if (result.success)
      toast.success(`连接成功，耗时 ${result.latencyMs} ms`)
    else
      toast.error(result.message)
  })
}

async function save() {
  if (saving.value || testingForm.value)
    return
  const name = form.name.trim()
  const proxyUrl = form.proxyUrl.trim()
  if (!name || (!editing.value && !proxyUrl)) {
    toast.warning('请填写代理名称和连接地址')
    return
  }
  const location = form.customLocation
    ? normalizeRequestLocation(form.location)
    : null
  const locationError = location ? requestLocationError(location) : ''
  if (locationError) {
    toast.warning(locationError)
    return
  }
  await saveAction.run(async () => {
    // 编辑时留空保留已保存的地址和认证，不能用脱敏地址覆盖原连接。
    await (editing.value
      ? updateProxy({
          id: editing.value.id,
          revision: editing.value.revision,
          name,
          proxyUrl: proxyUrl || undefined,
          location,
        })
      : createProxy({
          name,
          proxyUrl,
          location,
        }))
    showForm.value = false
    form.proxyUrl = ''
    toast.success('代理已保存')
    search.value = ''
    query.page.value = 1
    await query.execute()
  })
}

function requestDelete(proxy: OutboundProxyRecord) {
  pendingDelete.value = proxy
  showDelete.value = true
}

async function confirmDelete() {
  const proxy = pendingDelete.value
  if (!proxy || deleting.value)
    return
  await deleteAction.run(async () => {
    await deleteProxy({ id: proxy.id, revision: proxy.revision })
    showDelete.value = false
    await query.execute()
    toast.success('代理已删除')
  })
}

function setPage(page: number) {
  query.page.value = page
  void query.execute()
}

function setPageSize(size: number) {
  query.pageSize.value = size
  setPage(1)
}

watch(showForm, (open) => {
  if (!open) {
    form.proxyUrl = ''
  }
})
// 选择只对当前页有意义：翻页或换搜索条件后清空，避免对看不见的行执行批量操作。
watch([() => query.page.value, () => query.pageSize.value, search], () => {
  selectedIds.value = new Set()
})
watchDebounced(search, () => setPage(1), { debounce: 300 })
onMounted(() => void query.execute())
</script>

<template>
  <div class="flex h-full min-h-0 w-full flex-col overflow-hidden">
    <BasePageHeader
      class="h-17"
      title="代理管理"
      description="管理账号使用的代理，测试连接、出口地区与上游可达性"
    />
    <BaseCard class="mt-5 flex h-[calc(100dvh-136px)] min-h-125 flex-col">
      <template #header>
        <div class="flex w-full flex-col gap-3 sm:flex-row sm:items-center">
          <BaseInput v-model="search" class="sm:w-80" aria-label="搜索代理" placeholder="搜索代理名称...">
            <template #prefix>
              <Search class="size-4.5 text-cp-text-tertiary" />
            </template>
          </BaseInput>
          <div class="flex shrink-0 flex-wrap items-center justify-end gap-2 sm:ml-auto">
            <span v-if="selectedCount > 0" class="text-cp-xs text-cp-text-secondary">已选 {{ selectedCount }} 条</span>
            <template v-if="batch.running.value">
              <span class="text-cp-sm tabular-nums text-cp-text-secondary" aria-live="polite">
                {{ batch.kind.value === 'quality' ? '质量检测中' : '测试中' }} {{ batch.progressLabel.value }}
              </span>
              <BaseButton variant="secondary" @click="batch.cancel()">
                <template #icon>
                  <X class="size-4" />
                </template>
                取消
              </BaseButton>
            </template>
            <template v-else>
              <BaseButton variant="secondary" :title="selectedCount ? '测试已选代理' : '测试当前搜索条件下的全部代理'" @click="batchTest()">
                <template #icon>
                  <Wifi class="size-4" />
                </template>
                批量测试
              </BaseButton>
              <BaseButton variant="secondary" :title="selectedCount ? '检测已选代理' : '检测当前搜索条件下的全部代理'" @click="batchQuality()">
                <template #icon>
                  <ShieldCheck class="size-4" />
                </template>
                批量质量检测
              </BaseButton>
              <BaseButton variant="secondary" :disabled="selectedCount === 0" @click="showBatchDelete = true">
                <template #icon>
                  <Trash2 class="size-4" />
                </template>
                批量删除
              </BaseButton>
            </template>
            <BaseButton variant="secondary" :disabled="batch.running.value" @click="showBatchCreate = true">
              <template #icon>
                <ListPlus class="size-4" />
              </template>
              批量添加
            </BaseButton>
            <BaseButton variant="primary" @click="openForm()">
              <template #icon>
                <Plus class="size-4" />
              </template>
              新增代理
            </BaseButton>
          </div>
        </div>
      </template>
      <template #body>
        <div class="flex h-full min-h-0 flex-col">
          <BaseTable class="min-h-0 flex-1 [--cp-table-row-height:64px]" :columns="columns" :rows="proxies" :loading="loading" :selected-row-keys="selectedRowKeys" :empty-text="search.trim() ? '没有找到匹配的代理，请尝试其他名称' : '暂无代理，请点击新增代理添加'">
            <template #header-selection>
              <BaseCheckbox :model-value="allSelected" :indeterminate="indeterminate" label="选择当前页代理" @update:model-value="toggleAll" />
            </template>
            <template #selection="{ row }">
              <BaseCheckbox :model-value="selectedIds.has(row.id)" label="选择代理" @update:model-value="toggleSelection(row.id)" />
            </template>
            <template #name="{ row }">
              <span class="block truncate text-cp text-cp-text" :title="row.name">{{ row.name }}</span>
            </template>
            <template #address="{ row }">
              <div class="grid min-w-0 gap-1">
                <span class="flex min-w-0 items-center gap-1 text-cp-xs font-emphasis text-cp-text-quaternary">
                  <LockKeyhole v-if="row.hasAuthentication" class="size-3 shrink-0" aria-label="已保存代理认证" />
                  <span class="truncate font-mono" :title="row.endpoint">{{ row.endpoint }}</span>
                  <BasePopover :model-value="copyMenuId === row.id" placement="bottom-start" @update:model-value="copyMenuId = $event ? row.id : null">
                    <template #trigger>
                      <BaseIconButton size="sm" variant="ghost" label="复制代理地址" aria-haspopup="menu">
                        <Copy class="size-3" />
                      </BaseIconButton>
                    </template>
                    <div role="menu" class="grid w-64 gap-0.5 p-1.5">
                      <BaseMenuItem v-for="format in endpointCopyFormats(row.endpoint)" :key="format.value" role="menuitem" @click="copyEndpoint(format.value)">
                        <span class="truncate font-mono text-cp-xs">{{ format.label }}</span>
                      </BaseMenuItem>
                      <p v-if="row.hasAuthentication" class="m-0 px-3 pt-1 pb-1.5 text-cp-xs leading-relaxed text-cp-text-tertiary">
                        账号密码只保存在服务端，不会出现在复制内容里。
                      </p>
                    </div>
                  </BasePopover>
                </span>
                <span v-if="row.location" class="flex min-w-0 items-center gap-1 text-cp-xs text-cp-text-secondary" :title="`${row.location.country} / ${row.location.region} / ${row.location.city} · ${row.location.timezone}`">
                  <MapPin class="size-3 shrink-0" aria-hidden="true" />
                  <span class="truncate">{{ row.location.city }} · {{ row.location.timezone }}</span>
                </span>
              </div>
            </template>
            <template #exitIp="{ row }">
              <div v-if="row.lastTest && (row.lastTest.exitIpv4 || row.lastTest.exitIpv6 || row.lastTest.exitIp)" class="grid min-w-0 gap-0.5">
                <span v-if="row.lastTest.exitGeo" class="flex min-w-0 items-center gap-1.5 text-cp-sm text-cp-text" :title="[row.lastTest.exitGeo.country, row.lastTest.exitGeo.region, row.lastTest.exitGeo.city].filter(Boolean).join(' / ')">
                  <!-- Windows 不渲染国旗 emoji，统一用地区码标签，跨平台一致。 -->
                  <span class="shrink-0 rounded-cp-sm bg-cp-fill-secondary px-1 font-mono text-[10px] leading-4 font-bold text-cp-text-secondary">{{ row.lastTest.exitGeo.countryCode }}</span>
                  <span class="truncate">{{ exitGeoLabel(row.lastTest.exitGeo) }}</span>
                </span>
                <!-- 双栈出口逐条列出；无双栈字段时回落单一 exitIp。 -->
                <template v-if="row.lastTest.exitIpv4 || row.lastTest.exitIpv6">
                  <span v-if="row.lastTest.exitIpv4" class="break-all font-mono text-cp-xs text-cp-text-secondary" :title="`IPv4: ${row.lastTest.exitIpv4}`">{{ row.lastTest.exitIpv4 }}</span>
                  <span v-if="row.lastTest.exitIpv6" class="break-all font-mono text-cp-xs text-cp-text-secondary" :title="`IPv6: ${row.lastTest.exitIpv6}`">{{ row.lastTest.exitIpv6 }}</span>
                </template>
                <span v-else-if="row.lastTest.exitIp" class="break-all font-mono text-cp-xs text-cp-text-secondary">{{ row.lastTest.exitIp }}</span>
              </div>
              <span v-else class="text-cp-text-quaternary">-</span>
            </template>
            <template #latency="{ row }">
              <div class="grid justify-items-start gap-1">
                <span v-if="testingIds.has(row.id)" class="text-cp-text-secondary">测试中</span>
                <span v-else-if="row.lastTest?.success" class="font-mono tabular-nums" :class="latencyToneClass(row.lastTest)">
                  {{ row.lastTest.latencyMs }} ms
                </span>
                <span v-else-if="row.lastTest" class="text-cp-error-text" :title="`${row.lastTest.message}（耗时 ${row.lastTest.latencyMs} ms）`">失败</span>
                <span v-else class="text-cp-text-quaternary">未测试</span>
                <span v-if="qualityCheckingIds.has(row.id)" class="text-cp-xs text-cp-text-secondary">质量检测中</span>
                <button
                  v-else-if="row.quality"
                  type="button"
                  class="inline-flex cursor-pointer items-center gap-1 rounded-cp border-0 px-1.5 py-0.5 text-cp-xs font-bold outline-none transition-opacity hover:opacity-80 focus-visible:ring-2 focus-visible:ring-cp-control-outline"
                  :class="qualityStatus(row.quality.status).badge"
                  :title="`${row.quality.summary}，点击查看报告`"
                  @click="openReport(row)"
                >
                  <span>{{ qualityStatus(row.quality.status).label }}</span>
                  <span class="font-mono tabular-nums">{{ row.quality.grade }}/{{ row.quality.score }}</span>
                </button>
              </div>
            </template>
            <template #accounts="{ row }">
              <button type="button" class="inline-flex cursor-pointer items-center gap-1.5 rounded-sm border-0 bg-transparent p-0 text-cp-sm text-cp-text-secondary outline-none transition-colors hover:text-cp-primary-text focus-visible:ring-2 focus-visible:ring-cp-control-outline focus-visible:ring-offset-2 focus-visible:ring-offset-cp-bg-container" :aria-label="`查看 ${row.name} 的 ${row.accountCount} 个关联账号`" @click="inspected = row; showAccounts = true">
                <Users class="size-3.5" aria-hidden="true" />
                <span class="font-mono tabular-nums">{{ row.accountCount }}</span>
              </button>
            </template>
            <template #testedAt="{ row }">
              {{ row.lastTestAt ? formatDateTime(row.lastTestAt) : '-' }}
            </template>
            <template #actions="{ row }">
              <div class="flex items-center gap-1">
                <BaseIconButton size="sm" label="测试代理" :loading="testingIds.has(row.id)" :disabled="busyIds.has(row.id)" @click="checkProxy(row)">
                  <Wifi class="size-3.5 text-cp-link" />
                </BaseIconButton>
                <BaseIconButton size="sm" label="质量检测" :loading="qualityCheckingIds.has(row.id)" :disabled="busyIds.has(row.id)" @click="checkQuality(row)">
                  <ShieldCheck class="size-3.5 text-cp-link" />
                </BaseIconButton>
                <BaseIconButton size="sm" label="编辑代理" :disabled="busyIds.has(row.id)" @click="openForm(row)">
                  <Pencil class="size-3.5 text-cp-link" />
                </BaseIconButton>
                <BaseIconButton size="sm" :label="row.accountCount ? '代理正在被账号使用' : '删除代理'" :disabled="row.accountCount > 0 || busyIds.has(row.id)" @click="requestDelete(row)">
                  <Trash2 class="size-3.5 text-cp-error" />
                </BaseIconButton>
              </div>
            </template>
          </BaseTable>
          <BaseTablePagination :pagination="pagination" :loading="loading" @page-change="setPage" @page-size-change="setPageSize" />
        </div>
      </template>
    </BaseCard>

    <ProxyFormModal
      v-model="showForm"
      v-model:name="form.name"
      v-model:proxy-url="form.proxyUrl"
      v-model:custom-location="form.customLocation"
      v-model:location="form.location"
      :proxy="editing"
      :saving="saving"
      :testing="testingForm"
      @save="save"
      @test="testConnection"
    />
    <BaseConfirmModal v-model="showDelete" title="删除代理" destructive :loading="deleting" @confirm="confirmDelete">
      <p class="m-0">
        确定删除“{{ pendingDelete?.name }}”吗？
      </p>
    </BaseConfirmModal>
    <BaseConfirmModal v-model="showBatchDelete" title="批量删除代理" destructive :loading="batchDeleting" @confirm="confirmBatchDelete">
      <p class="m-0">
        确定删除已选的 {{ selectedCount }} 条代理吗？仍有账号使用的代理会被跳过。
      </p>
    </BaseConfirmModal>
    <ProxyAccountsModal v-model="showAccounts" :proxy="inspected" @removed="query.execute({ silent: true })" />
    <ProxyBatchCreateModal v-model="showBatchCreate" @created="handleBatchCreated" />
    <ProxyQualityReportModal
      v-model="showReport"
      :proxy="reportProxy"
      :report="freshReport"
      :checking="reportProxy !== null && qualityCheckingIds.has(reportProxy.id)"
      @recheck="checkQuality"
    />
  </div>
</template>
