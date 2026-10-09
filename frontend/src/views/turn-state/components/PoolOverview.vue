<script setup lang="ts">
import type { PoolGateway, PoolSnapshot } from '@/api'
import { BaseButton, BaseCard, BaseInput, BaseModal } from '@codex-proxy/ui'
import { computed, ref } from 'vue'
import { formatDateTime } from '@/utils/format'
import { remaining, verificationText } from '../utils/poolOverview'
import AccountPoolSummary from './AccountPoolSummary.vue'

const props = defineProps<{ snapshot: PoolSnapshot | null, loading: boolean, stale: boolean, paused: boolean, error: string, now: number }>()
const emit = defineEmits<{ refresh: [] }>()
const search = ref('')
const filter = ref('all')
const selectedGateway = ref('')
const detailOpen = ref(false)
const groups = [
  { value: 'all', label: '全部' },
  { value: 'verified', label: '有已验证连接' },
  { value: 'unchecked', label: '仅未验证连接' },
  { value: 'unavailable', label: '暂无可用连接' },
]
function category(gateway: PoolGateway) {
  return gateway.verifiedConnections > 0 ? 'verified' : gateway.availableConnections > 0 ? 'unchecked' : 'unavailable'
}
const accounts = computed(() => Object.fromEntries(props.snapshot?.accounts.map(account => [account.accountId, account]) ?? []))
const rows = computed(() => {
  const query = search.value.trim().toLowerCase()
  return (props.snapshot?.gateways ?? []).filter(gateway =>
    (filter.value === 'all' || category(gateway) === filter.value)
    && (!query || [gateway.gateway, ...gateway.models, ...gateway.accountIds.map(id => accounts.value[id]?.name ?? '')].join(' ').toLowerCase().includes(query)),
  )
})
const selected = computed(() => props.snapshot?.gateways.find(gateway => gateway.gateway === selectedGateway.value))
const selectedAccounts = computed(() => selected.value?.accountIds.map(id => accounts.value[id]).filter(account => account !== undefined) ?? [])
const metrics = computed(() => [
  { label: '已观察网关', value: props.snapshot?.totals.gateways, unit: '个' },
  { label: '有效票据', value: props.snapshot?.totals.tickets, unit: '张' },
  { label: '可复用连接', value: props.snapshot?.totals.availableConnections, unit: '条' },
  { label: '验证有效连接', value: props.snapshot?.totals.verifiedConnections, unit: '条' },
  { label: '正在准备', value: props.snapshot?.totals.preparingAccounts, unit: '个账号' },
  { label: '冷却中', value: props.snapshot?.totals.coolingAccounts, unit: '个账号' },
])
const snapshotState = computed(() => props.error ? '读取失败' : !props.snapshot ? '正在读取' : props.paused ? '已暂停刷新' : props.stale ? '快照已过期' : '自动刷新中')
function open(gateway: PoolGateway) {
  selectedGateway.value = gateway.gateway
  detailOpen.value = true
}
function tone(gateway: PoolGateway) {
  if (props.stale)
    return 'bg-cp-fill-quaternary text-cp-text-secondary'
  if (gateway.verifiedConnections > 0)
    return 'bg-cp-green-container text-cp-green-on-container'
  if (gateway.availableConnections > 0)
    return 'bg-cp-orange-container text-cp-orange-on-container'
  return 'bg-cp-fill-quaternary text-cp-text-secondary'
}
</script>

<template>
  <BaseCard title="网关总览" description="本实例已观察到的网关与空闲预热连接" data-testid="pool-overview" :data-snapshot-stale="stale" :data-snapshot-paused="paused" :data-snapshot-now="now" :data-snapshot-until="snapshot?.validUntilMs">
    <div class="grid min-w-0 gap-4">
      <div class="flex flex-wrap items-center justify-between gap-2 text-cp-xs text-cp-text-secondary">
        <span role="status" data-testid="pool-snapshot-status">{{ snapshotState }}<template v-if="snapshot"> · {{ formatDateTime(new Date(snapshot.observedAtMs).toISOString()) }}</template></span>
        <BaseButton size="sm" variant="soft" :loading="loading" @click="emit('refresh')">
          刷新票池
        </BaseButton>
      </div>
      <div class="grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6" aria-label="票池指标">
        <div v-for="metric in metrics" :key="metric.label" class="rounded-cp bg-cp-fill-quaternary px-3 py-3">
          <div class="text-cp-xs text-cp-text-secondary">
            {{ metric.label }}
          </div>
          <div class="mt-1 flex items-baseline gap-1.5">
            <strong class="font-mono text-xl font-semibold tabular-nums">{{ stale ? '—' : metric.value ?? '—' }}</strong>
            <span class="text-cp-xs text-cp-text-secondary">{{ metric.unit }}</span>
          </div>
        </div>
      </div>
      <p v-if="error" class="m-0 text-cp-sm text-cp-error" role="alert">
        {{ error }}，已保留上次快照
      </p>
      <div class="flex flex-wrap items-center gap-2">
        <div class="flex flex-wrap gap-1 rounded-cp bg-cp-fill-quaternary p-1" aria-label="筛选网关状态">
          <button
            v-for="group in groups" :key="group.value" type="button" class="rounded-cp px-2.5 py-1.5 text-cp-xs transition-colors focus-visible:outline-2 focus-visible:outline-cp-primary"
            :class="filter === group.value ? 'bg-cp-bg-container text-cp-text shadow-cp-card' : 'text-cp-text-secondary hover:bg-cp-fill-tertiary'"
            :aria-pressed="filter === group.value" @click="filter = group.value"
          >
            {{ group.label }}
          </button>
        </div>
        <BaseInput v-model="search" class="w-full sm:ml-auto sm:w-64" placeholder="搜索网关、账号或模型" aria-label="搜索网关、账号或模型" />
      </div>
      <div v-if="!snapshot && loading" class="py-7 text-center text-cp-sm text-cp-text-secondary">
        正在读取票据与连接状态
      </div>
      <div v-else-if="!rows.length" class="rounded-cp bg-cp-fill-quaternary py-7 text-center text-cp-sm text-cp-text-secondary" data-testid="pool-empty">
        {{ !snapshot ? '暂时无法取得票池状态，请重试' : search || filter !== 'all' ? '没有匹配的网关' : '尚未观察到网关，启用账号的票与预热后会在这里显示' }}
      </div>
      <div v-else class="grid grid-cols-1 gap-2.5 sm:grid-cols-2 xl:grid-cols-4" aria-label="网关列表">
        <button
          v-for="gateway in rows" :key="gateway.gateway" type="button" class="grid min-w-0 gap-2 rounded-cp p-3 text-left transition-opacity hover:opacity-85 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-cp-primary"
          :class="tone(gateway)" :aria-label="`查看 ${gateway.gateway}`" data-testid="pool-gateway-card" @click="open(gateway)"
        >
          <div class="flex items-center justify-between gap-2">
            <strong class="truncate font-mono text-cp-sm">{{ gateway.gateway }}</strong>
            <span class="shrink-0 text-cp-xs">{{ gateway.accountIds.length }} 个账号</span>
          </div>
          <div class="text-cp-sm">
            {{ stale ? '状态待刷新' : `已验证 ${gateway.verifiedConnections} / 可复用 ${gateway.availableConnections}` }}
          </div>
          <div class="flex items-center justify-between gap-2 text-cp-xs opacity-80">
            <span>{{ stale ? '票据 —' : `${gateway.tickets} 张有效票` }}</span>
            <span>{{ stale ? '时效未知' : gateway.nextExpiryMs ? `最近到期 ${remaining(gateway.nextExpiryMs, now)}` : '无可用资源' }}</span>
          </div>
          <div class="truncate font-mono text-[11px] opacity-75" :title="gateway.models.join(' · ')">
            {{ gateway.models.join(' · ') || '模型未记录' }}
          </div>
        </button>
      </div>
      <div class="flex flex-wrap justify-between gap-2 text-cp-xs text-cp-text-secondary">
        <span>验证结果按账号和模型区分</span><span v-if="snapshot">显示 {{ rows.length }} / {{ snapshot.gateways.length }} 个网关</span>
      </div>
    </div>
  </BaseCard>
  <BaseModal v-model="detailOpen" :title="selectedGateway" size="md-wide">
    <div v-if="selected" class="grid gap-4" data-testid="pool-gateway-detail">
      <p v-if="stale || paused" class="m-0 text-cp-sm text-cp-warning">
        {{ snapshotState }}，以下为最近一次快照
      </p>
      <section v-for="account in selectedAccounts" :key="account.accountId" class="grid min-w-0 gap-3 rounded-cp bg-cp-fill-quaternary p-3">
        <div class="flex flex-wrap items-start justify-between gap-2">
          <strong class="break-all text-cp-sm">{{ account.name }}</strong>
          <AccountPoolSummary :account="account" :stale="stale" :paused="paused" :now="now" expanded />
        </div>
        <div v-for="connection in account.connections.filter(item => item.gateway === selectedGateway)" :key="connection.id" class="grid gap-1 rounded-cp bg-cp-bg-container p-3 text-cp-sm">
          <div class="flex flex-wrap items-center justify-between gap-2">
            <span class="font-mono">{{ connection.model }}</span><span>{{ stale ? '验证状态待刷新' : verificationText[connection.verification] }}</span>
          </div>
          <span class="text-cp-xs text-cp-text-secondary">连接剩余 {{ stale ? '未知' : remaining(connection.expiresAtMs, now) }} · 验证剩余 {{ stale ? '未知' : remaining(connection.verificationExpiresAtMs, now) }}</span>
          <span v-if="connection.verifiedAtMs" class="text-cp-xs text-cp-text-secondary">验证于 {{ formatDateTime(new Date(connection.verifiedAtMs).toISOString()) }}</span>
        </div>
        <div v-for="(ticket, index) in account.tickets.filter(item => item.gateway === selectedGateway)" :key="`${ticket.model}-${index}`" class="flex flex-wrap justify-between gap-2 text-cp-xs text-cp-text-secondary">
          <span class="font-mono">{{ ticket.model }}</span><span>票据剩余 {{ stale ? '未知' : remaining(ticket.expiresAtMs, now) }}</span>
        </div>
      </section>
    </div>
    <p v-else class="m-0 text-cp-sm text-cp-text-secondary">
      该网关已不在当前快照中
    </p>
  </BaseModal>
</template>
