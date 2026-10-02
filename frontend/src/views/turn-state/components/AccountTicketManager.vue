<script setup lang="ts">
import type { Account, OAuthStateConfiguration, TurnStateAutoHunt } from '@/api'
import { BaseButton, BaseCard, BaseConfirmModal, BaseInput, BaseTable, BaseTablePagination, defineTableColumns, toast } from '@codex-proxy/ui'
import { watchDebounced } from '@vueuse/core'
import { computed, onMounted, ref } from 'vue'
import { getAccounts, mintAccountTurnState, updateAccountTurnState } from '@/api'
import SyncedSwitch from '@/components/SyncedSwitch.vue'
import { useAccountConfigurations } from '@/composables/useAccountConfigurations'
import { usePagedQuery } from '@/composables/usePagedQuery'
import { useUiClock } from '@/composables/useUiClock'
import { formatDateTime } from '@/utils/format'
import AccountTicketDetails from './AccountTicketDetails.vue'

const emit = defineEmits<{ changed: [] }>()
const search = ref('')
const query = usePagedQuery({
  initialPageSize: 20,
  load: (pagination, options) => getAccounts({ ...pagination, provider: 'openai', search: search.value.trim() }, options),
})
const { items: accounts, loading, error } = query
const { entries, reload: reloadConfigurations } = useAccountConfigurations(accounts)
const pagination = computed(() => ({ currentPage: query.page.value, pageSize: query.pageSize.value, total: query.total.value }))
const now = useUiClock()
const busyId = ref<string | null>(null)
const huntRunning = ref(false)
const busy = computed(() => busyId.value !== null || huntRunning.value)
const selected = ref<Account | null>(null)
const detailsOpen = ref(false)
const recaptureOpen = ref(false)
const selectedConfiguration = computed(() => selected.value ? entries.value[selected.value.id]?.value : undefined)

const columns = defineTableColumns<Account>([
  { key: 'identity', label: '账号', kind: 'custom', size: '2xl' },
  { key: 'participation', label: '票与预热', kind: 'custom', size: 'sm' },
  { key: 'ticketState', label: '票据', kind: 'custom', size: 'xl' },
  { key: 'warmState', label: '预热连接', kind: 'custom', size: 'lg' },
  { key: 'actions', label: '操作', kind: 'custom', size: 'xl' },
])

function configuration(account: Account) {
  return entries.value[account.id]?.value
}
function activePins(config?: OAuthStateConfiguration) {
  return config?.turnStatePins.filter(pin => Date.parse(pin.expiresAt) > now.value.getTime()) ?? []
}
function ticketExpiry(config?: OAuthStateConfiguration) {
  return activePins(config).map(pin => pin.expiresAt).sort()[0]
}
function warmText(config: OAuthStateConfiguration) {
  if (!config.pinTurnState)
    return '未参与'
  if (!config.warmPool)
    return '未记录'
  if (!config.warmPool.enabled)
    return '全局未启用'
  return `可用 ${config.warmPool.held} 条`
}
const verdictText = { verified: '最近验证通过', ready: '最近未检查答案', degraded: '最近检查未通过', failed: '最近探测失败' }

async function refresh() {
  if (await query.execute())
    await reloadConfigurations()
}
function changePage(page: number) {
  query.page.value = page
  void refresh()
}
function changePageSize(size: number) {
  query.pageSize.value = size
  changePage(1)
}
watchDebounced(search, () => changePage(1), { debounce: 300 })
onMounted(() => void query.execute())

async function perform(account: Account, action: () => Promise<void>) {
  if (busy.value)
    return
  busyId.value = account.id
  try {
    await action()
  }
  catch (cause) {
    toast.error(cause instanceof Error ? cause.message : '操作失败')
  }
  finally {
    await reloadConfigurations()
    busyId.value = null
    emit('changed')
  }
}
async function toggle(account: Account, enabled: boolean) {
  await perform(account, async () => {
    await updateAccountTurnState({ accountId: account.id, pinTurnState: enabled })
    toast.success(enabled ? '已启用票与预热' : '已停用票与预热')
  })
}
async function mint(account: Account) {
  await perform(account, async () => {
    const result = await mintAccountTurnState({ accountId: account.id })
    toast.success(result.observeOnly ? '打票完成，仅观测' : `打票完成，获得 ${result.tickets.length} 项票据`)
  })
}
function openDetails(account: Account) {
  selected.value = account
  detailsOpen.value = true
}
async function recapture() {
  const account = selected.value
  if (!account)
    return
  recaptureOpen.value = false
  await perform(account, async () => {
    await updateAccountTurnState({ accountId: account.id, pinTurnState: true })
    toast.success('已清除旧绑定，等待重新捕获')
  })
}
async function setAutoHunt(autoRenew: TurnStateAutoHunt | null) {
  const account = selected.value
  if (!account)
    return
  await perform(account, async () => {
    // 只提交续期参数，避免再次写开关导致刚捕获的票失效。
    await updateAccountTurnState({ accountId: account.id, turnStateAutoHunt: autoRenew ? { enabled: true, ...autoRenew } : { enabled: false } })
  })
}
async function afterHunt(_boundChanged: boolean, autoRenew: TurnStateAutoHunt | null) {
  if (autoRenew || selectedConfiguration.value?.turnStateAutoHunt)
    await setAutoHunt(autoRenew)
  await refresh()
  emit('changed')
}
async function afterHuntCancelled() {
  // 取消时服务端可能仍在完成提交，等待收尾后回读实际状态。
  await new Promise(resolve => setTimeout(resolve, 1500))
  await refresh()
  emit('changed')
}
</script>

<template>
  <BaseCard title="账号票与预热" description="开关即时生效，仅支持 OpenAI OAuth 账号">
    <div class="mb-3 flex flex-wrap items-center gap-3">
      <BaseInput v-model="search" aria-label="搜索票据账号" placeholder="搜索账号" class="w-full sm:w-64" :disabled="busy" />
      <BaseButton size="sm" variant="secondary" :loading="loading" :disabled="busy" @click="refresh">
        刷新账号
      </BaseButton>
    </div>
    <p v-if="error" class="text-cp-sm text-cp-error" role="alert">
      {{ error }}
    </p>
    <div class="max-h-96 min-w-0 overflow-auto">
      <BaseTable :columns="columns" :rows="accounts" :loading="loading" density="compact" empty-text="暂无匹配账号">
        <template #identity="{ row }">
          <div class="max-w-64 truncate font-emphasis" :title="row.name">
            {{ row.name }}
          </div>
          <span class="text-cp-xs text-cp-text-secondary">{{ row.authenticationKind === 'oauth' ? (row.enabled ? '调度已启用' : '调度已停用') : 'API Key 账号不适用' }}</span>
        </template>
        <template #participation="{ row }">
          <SyncedSwitch
            v-if="row.authenticationKind === 'oauth'"
            :model-value="configuration(row)?.pinTurnState ?? false" :label="`${row.name} 票与预热`"
            :disabled="busy || !configuration(row) || entries[row.id]?.loading" @update:model-value="toggle(row, $event)"
          />
          <span v-else class="text-cp-text-secondary">不适用</span>
        </template>
        <template #ticketState="{ row }">
          <template v-if="configuration(row)">
            <span>{{ !configuration(row)?.pinTurnState ? '未启用' : activePins(configuration(row)).length ? `有效票 ${activePins(configuration(row)).length}` : '暂无有效票' }}</span>
            <div v-if="ticketExpiry(configuration(row))" class="text-cp-xs text-cp-text-secondary">
              {{ formatDateTime(ticketExpiry(configuration(row))!) }} 到期
            </div>
          </template>
          <span v-else-if="row.authenticationKind !== 'oauth'">—</span>
          <span v-else :class="entries[row.id]?.error ? 'text-cp-error' : 'text-cp-text-secondary'">{{ entries[row.id]?.error ? '读取失败' : '读取中' }}</span>
        </template>
        <template #warmState="{ row }">
          <template v-if="configuration(row)">
            <span>{{ warmText(configuration(row)!) }}</span>
            <div v-if="configuration(row)?.pinTurnState && configuration(row)?.warmPool?.last?.verdict" class="text-cp-xs text-cp-text-secondary">
              {{ verdictText[configuration(row)!.warmPool!.last!.verdict!] }}
            </div>
          </template>
          <span v-else>—</span>
        </template>
        <template #actions="{ row }">
          <div v-if="row.authenticationKind === 'oauth'" class="flex items-center gap-2">
            <BaseButton size="sm" :loading="busyId === row.id" :disabled="busy || entries[row.id]?.loading || !configuration(row)?.pinTurnState || !configuration(row)?.cloudMint?.enabled" @click="mint(row)">
              立即打票
            </BaseButton>
            <BaseButton size="sm" variant="soft" :disabled="busy || !configuration(row)" @click="openDetails(row)">
              详情
            </BaseButton>
          </div>
        </template>
      </BaseTable>
    </div>
    <BaseTablePagination :pagination="pagination" :loading="loading || busy" @page-change="changePage" @page-size-change="changePageSize" />
  </BaseCard>
  <AccountTicketDetails
    v-if="selected && detailsOpen" :key="selected.id" v-model="detailsOpen" :account="selected" :configuration="selectedConfiguration"
    :busy="busyId !== null" @hunt-busy="huntRunning = $event" @recapture="recaptureOpen = true"
    @hunted="afterHunt" @hunt-cancelled="afterHuntCancelled" @stop-auto-hunt="setAutoHunt(null)"
  />
  <BaseConfirmModal v-if="recaptureOpen" v-model="recaptureOpen" title="清除旧票并重新捕获" description="该账号现有绑定将失效，新票由后续请求或自动打票补充" confirm-text="重新捕获" :loading="busyId !== null" @confirm="recapture" />
</template>
