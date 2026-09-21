<script setup lang="ts">
import type { AccountCostRow, CostDay, CostTotals } from '@/api'
import { Pencil, PowerOff, RotateCcw } from '@lucide/vue'
import { usePreferredReducedMotion } from '@vueuse/core'
import dayjs from 'dayjs'
import { computed, onMounted, ref, shallowRef, watch } from 'vue'
import { getAccounts, getCostAccounts, getCostDaily, setAccountsRetired } from '@/api'
import AccountPurchaseModal from '@/components/AccountPurchaseModal.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSkeleton from '@/components/base/BaseSkeleton.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import { defineTableColumns } from '@/components/base/BaseTable/columns'
import BaseTable from '@/components/base/BaseTable/index.vue'
import { toast } from '@/components/base/BaseToast'
import BaseChart from '@/components/charts/BaseChart.vue'
import { useChartPalette } from '@/composables/useChartPalette'
import { formatDateTime } from '@/utils/date'
import { formatCompactNumber, formatInteger } from '@/utils/number'
import { accountCostState, accountCostStateLabels, costChartOption, formatCostPerUsd, formatMoney } from './presenter'

interface PurchaseTarget { id: string, label: string, addedAt?: string | null }

const rangeOptions = [
  { label: '今天', value: '1' },
  { label: '近 7 天', value: '7' },
  { label: '近 30 天', value: '30' },
  { label: '近 90 天', value: '90' },
]
const rangeDays = ref('7')
const showRetired = shallowRef(false)
const days = shallowRef<CostDay[]>([])
const totals = shallowRef<CostTotals | null>(null)
const accounts = shallowRef<AccountCostRow[]>([])
const unpriced = shallowRef<PurchaseTarget[]>([])
const loading = shallowRef(false)
const failed = shallowRef(false)
const retiringIds = ref(new Set<string>())
const showPurchase = shallowRef(false)
const purchaseTarget = shallowRef<PurchaseTarget | null>(null)
const purchaseRecord = shallowRef<AccountCostRow | null>(null)
const { palette } = useChartPalette()
const reducedMotion = usePreferredReducedMotion()
let requestVersion = 0

const range = computed(() => {
  const to = dayjs()
  return { from: to.subtract(Number(rangeDays.value) - 1, 'day').format('YYYY-MM-DD'), to: to.format('YYYY-MM-DD') }
})
// 最新的日期排在前面：每天打开页面首先要看的是今天。
const dailyRows = computed(() => [...days.value].reverse())
const chartOption = computed(() => costChartOption(days.value, palette.value, reducedMotion.value === 'reduce'))
const hasChartData = computed(() => days.value.some(day => day.spend > 0 || day.usageUsd > 0))

const dailyColumns = defineTableColumns<CostDay>([
  { key: 'day', label: '日期', kind: 'mono', size: 'sm' },
  { key: 'purchasedCount', label: '新购号数', kind: 'numeric', size: 'xs' },
  { key: 'spend', label: '投入', kind: 'numeric', size: 'sm', format: value => formatMoney(value as number) },
  { key: 'usageUsd', label: '跑出（USD）', kind: 'numeric', size: 'sm', format: value => formatMoney(value as number) },
  { key: 'costPerUsd', label: '每 1 刀成本', kind: 'custom', size: 'sm', align: 'right' },
  { key: 'cumulativeCostPerUsd', label: '区间累计', kind: 'numeric', size: 'sm', format: value => formatCostPerUsd(value as number | null) },
  { key: 'requestCount', label: '请求数', kind: 'numeric', size: 'sm', format: value => formatInteger(value as number) },
])
const accountColumns = defineTableColumns<AccountCostRow>([
  { key: 'account', label: '账号', kind: 'identity', size: 'xl' },
  { key: 'purchasedAt', label: '购买时间', kind: 'custom', size: 'md' },
  { key: 'price', label: '价格', kind: 'numeric', size: 'xs', format: value => formatMoney(value as number | null) },
  { key: 'usageUsd', label: '跑出（USD）', kind: 'numeric', size: 'sm', format: value => formatMoney(value as number) },
  { key: 'costPerUsd', label: '每 1 刀成本', kind: 'custom', size: 'sm', align: 'right' },
  { key: 'totalTokens', label: 'Tokens', kind: 'numeric', size: 'xs', format: value => formatCompactNumber(value as number) },
  { key: 'state', label: '状态', kind: 'custom', size: 'sm' },
  { key: 'actions', label: '操作', kind: 'actions', size: 'sm', fixedWidth: true },
])

/** 均值线以上偏贵、以下划算；没有区间均值时不着色。 */
function ratioClass(value: number | null) {
  const average = totals.value?.costPerUsd
  if (value == null || average == null)
    return 'text-cp-text'
  return value > average * 1.15 ? 'text-cp-error-text' : value < average * 0.85 ? 'text-cp-success-text' : 'text-cp-text'
}

async function load(silent = false) {
  const version = ++requestVersion
  if (!silent)
    loading.value = true
  failed.value = false
  try {
    const [daily, rows, directory] = await Promise.all([
      getCostDaily(range.value),
      getCostAccounts({ ...range.value, includeRetired: showRetired.value }),
      // 还没录价的 Business/Team 号：提醒补录，否则它们的投入不会进入核算。
      getAccounts({ page: 1, pageSize: 200, search: '' }, { silent: true }).catch(() => null),
    ])
    if (version !== requestVersion)
      return
    days.value = daily.days
    totals.value = daily.totals
    accounts.value = rows.items
    const priced = new Set(rows.items.filter(row => row.price != null).map(row => row.accountId))
    unpriced.value = (directory?.items ?? [])
      .filter(account => account.planTypeDisplay === 'Business' && !priced.has(account.id))
      .map(account => ({ id: account.id, label: account.email ?? account.name, addedAt: account.addedAt }))
  }
  catch {
    if (version === requestVersion)
      failed.value = true
  }
  finally {
    if (version === requestVersion)
      loading.value = false
  }
}

function openPurchase(target: PurchaseTarget, record: AccountCostRow | null = null) {
  purchaseTarget.value = target
  purchaseRecord.value = record
  showPurchase.value = true
}

async function toggleRetired(row: AccountCostRow) {
  if (retiringIds.value.has(row.accountId))
    return
  retiringIds.value.add(row.accountId)
  const retired = row.retiredAt == null
  try {
    await setAccountsRetired({ accountIds: [row.accountId], retired })
    toast.success(retired ? '已标记下线' : '已恢复上线')
    await load(true)
  }
  catch {}
  finally {
    retiringIds.value.delete(row.accountId)
  }
}

watch([rangeDays, showRetired], () => void load())
onMounted(() => void load())
</script>

<template>
  <div class="flex w-full flex-col gap-5 pb-6">
    <BasePageHeader class="h-17" title="成本核算" description="按天核对买号的投入和跑出的金额；价格与美元按 1:1 对比" />

    <div class="flex flex-wrap items-center gap-3">
      <BaseSegmented v-model="rangeDays" label="核算区间" :options="rangeOptions" class="w-80 max-w-full" />
      <span class="text-cp-xs text-cp-text-tertiary">{{ range.from }} 至 {{ range.to }}，按东八区自然日</span>
    </div>

    <section class="grid grid-cols-2 gap-3 lg:grid-cols-4" aria-label="区间合计">
      <div
        v-for="card in [
          { label: '新购号数', value: totals ? formatInteger(totals.purchasedCount) : '—', hint: '区间内购入且已录价' },
          { label: '投入', value: formatMoney(totals?.spend), hint: '购买价格合计' },
          { label: '跑出（USD）', value: formatMoney(totals?.usageUsd), hint: '按模型价格折算' },
          { label: '每 1 刀成本', value: formatCostPerUsd(totals?.costPerUsd), hint: '投入 ÷ 跑出' },
        ]" :key="card.label" class="rounded-lg bg-cp-bg-container p-4 shadow-cp-tertiary"
      >
        <p class="m-0 text-cp-xs font-emphasis text-cp-text-secondary">
          {{ card.label }}
        </p>
        <BaseSkeleton v-if="loading && !totals" shape="text" class="mt-3 h-6 w-24" />
        <p v-else class="m-0 mt-1.5 font-mono text-2xl leading-none font-heavy tabular-nums text-cp-text">
          {{ card.value }}
        </p>
        <p class="m-0 mt-2 text-cp-xs text-cp-text-tertiary">
          {{ card.hint }}
        </p>
      </div>
    </section>

    <div v-if="failed" class="grid place-items-center gap-3 rounded-lg bg-cp-bg-container p-10 text-center shadow-cp-tertiary">
      <p class="m-0 text-cp-sm text-cp-text-secondary">
        核算数据加载失败
      </p>
      <BaseButton variant="secondary" @click="load()">
        重新加载
      </BaseButton>
    </div>

    <template v-else>
      <section v-if="unpriced.length > 0" class="rounded-lg bg-cp-warning-container p-4" aria-label="待录价账号">
        <p class="m-0 text-cp-sm font-heavy text-cp-warning-on-container">
          {{ unpriced.length }} 个 Business 号还没录入购买价格，它们的投入不会计入核算
        </p>
        <div class="mt-2.5 flex flex-wrap gap-2">
          <BaseButton v-for="account in unpriced.slice(0, 12)" :key="account.id" size="sm" variant="secondary" @click="openPurchase(account)">
            {{ account.label }}
          </BaseButton>
          <span v-if="unpriced.length > 12" class="self-center text-cp-xs text-cp-warning-on-container">另有 {{ unpriced.length - 12 }} 个，可在账号管理里录入</span>
        </div>
      </section>

      <BaseCard title="每日核算" description="投入记在购买当天，跑出记在实际发生的当天">
        <div class="grid gap-4">
          <BaseSkeleton v-if="loading && days.length === 0" class="h-56 w-full" />
          <BaseChart v-else-if="hasChartData" :option="chartOption" :height="224" />
          <p v-else class="m-0 grid h-32 place-items-center text-cp-sm text-cp-text-tertiary">
            这个区间还没有投入或跑出。先给 team 号录入购买价格。
          </p>
          <div class="max-h-96 min-w-0">
            <BaseTable :columns="dailyColumns" :rows="dailyRows" row-key="day" density="compact" :loading="loading && days.length === 0" empty-text="暂无数据">
              <template #costPerUsd="{ row }">
                <span class="font-mono font-heavy tabular-nums" :class="ratioClass(row.costPerUsd)">{{ formatCostPerUsd(row.costPerUsd) }}</span>
              </template>
            </BaseTable>
          </div>
        </div>
      </BaseCard>

      <BaseCard title="单号核算" description="每个号在所选区间内跑出的金额与它的购买价格；下线由你手动标记">
        <template #actions>
          <BaseSwitch v-model="showRetired" label="显示已下线" show-label />
        </template>
        <div class="max-h-[32rem] min-w-0">
          <BaseTable :columns="accountColumns" :rows="accounts" row-key="accountId" density="compact" :loading="loading && accounts.length === 0" :empty-text="showRetired ? '还没有录入过购买价格' : '没有在线的已录价账号，可打开「显示已下线」查看全部'">
            <template #account="{ row }">
              <div class="grid min-w-0 gap-0.5" :class="row.retiredAt ? 'opacity-60' : undefined">
                <strong class="truncate text-cp-sm text-cp-text" :title="row.email ?? row.name">{{ row.email ?? row.name }}</strong>
                <span v-if="row.note" class="truncate text-cp-xs text-cp-text-tertiary" :title="row.note">{{ row.note }}</span>
              </div>
            </template>
            <template #purchasedAt="{ row }">
              <span class="font-mono text-cp-xs tabular-nums text-cp-text-secondary">{{ formatDateTime(row.purchasedAt) }}</span>
            </template>
            <template #costPerUsd="{ row }">
              <!-- 还在跑的号比值只会继续下降，不拿它和均值比，避免刚上线的号一片红。 -->
              <span
                class="font-mono font-heavy tabular-nums"
                :class="accountCostState(row) === 'running' ? 'text-cp-text-tertiary' : ratioClass(row.costPerUsd)"
                :title="accountCostState(row) === 'running' ? '还在跑，跑得越多这个数越低' : undefined"
              >
                {{ formatCostPerUsd(row.costPerUsd) }}
              </span>
            </template>
            <template #state="{ row }">
              <span class="rounded-cp px-1.5 py-0.5 text-cp-xs font-bold" :class="accountCostStateLabels[accountCostState(row)].badge">
                {{ accountCostStateLabels[accountCostState(row)].label }}
              </span>
            </template>
            <template #actions="{ row }">
              <div class="flex items-center gap-1">
                <BaseIconButton size="sm" label="修改价格与备注" @click="openPurchase({ id: row.accountId, label: row.email ?? row.name }, row)">
                  <Pencil class="size-3.5 text-cp-link" />
                </BaseIconButton>
                <BaseIconButton size="sm" :label="row.retiredAt ? '恢复上线' : '标记下线'" :loading="retiringIds.has(row.accountId)" :disabled="retiringIds.has(row.accountId)" @click="toggleRetired(row)">
                  <RotateCcw v-if="row.retiredAt" class="size-3.5 text-cp-link" />
                  <PowerOff v-else class="size-3.5 text-cp-text-secondary" />
                </BaseIconButton>
              </div>
            </template>
          </BaseTable>
        </div>
      </BaseCard>
    </template>

    <AccountPurchaseModal v-model="showPurchase" :account="purchaseTarget" :purchase="purchaseRecord" @saved="load(true)" />
  </div>
</template>
