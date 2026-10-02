<script setup lang="ts">
// 经营日报：后台任务每 10 分钟按北京时间自然日汇总 CPR 号池投入与 sub2api 消费、收款。
// 平台 1 元 = 1 刀；英雄套餐订阅按 1/5 折算为「调整后」。毛利 = Codex 经 CPR 调整后消费 − 当天人民币买入成本。
import type { EChartsOption } from 'echarts'
import { RefreshCw } from '@lucide/vue'
import { computed, onBeforeUnmount, onMounted, shallowRef } from 'vue'
import request from '@/api/request'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import BaseChart from '@/components/charts/BaseChart.vue'
import { chartTooltipStyle } from '@/components/charts/tooltip'
import { useChartPalette } from '@/composables/useChartPalette'
import { formatDateTime } from '@/utils/date'

interface Slice { requests: number, standard: number, adjusted: number }
interface Sub2api {
  activeUsers: number
  requests: number
  standard: number
  actual: number
  adjusted: number
  codex: Slice
  codexViaCpr: Slice
  payments: number
}
interface OpsDay {
  day: string
  newAccounts: number
  purchasedAccounts: number
  purchaseCny: number
  purchaseUsd: number
  cpr: { requests: number, failedRequests: number, officialUsd: number }
  sub2api: Sub2api | null
  grossProfit: number | null
  refreshedAt: string
  finalized: boolean
}
interface OpsReport { sub2apiConfigured: boolean, days: OpsDay[] }

const REFRESH_MS = 60_000
const rangeOptions = [
  { label: '最近 7 天', value: '7' },
  { label: '最近 30 天', value: '30' },
  { label: '最近 90 天', value: '90' },
]
const range = shallowRef('30')
const report = shallowRef<OpsReport | null>(null)
const loading = shallowRef(false)
const error = shallowRef('')
let timer: ReturnType<typeof setInterval> | undefined

async function load() {
  loading.value = true
  try {
    report.value = await request<OpsReport>({ url: '/api/admin/ops-report/daily', method: 'GET', params: { days: Number(range.value) } })
    error.value = ''
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : '加载失败'
  }
  finally {
    loading.value = false
  }
}

onMounted(() => {
  void load()
  timer = setInterval(() => void load(), REFRESH_MS)
})
onBeforeUnmount(() => clearInterval(timer))

const days = computed(() => report.value?.days ?? [])
const today = computed(() => days.value[0] ?? null)
const ascending = computed(() => [...days.value].reverse())

function yuan(value: number | null | undefined) {
  if (value === null || value === undefined)
    return '—'
  return `¥${value.toLocaleString('zh-CN', { minimumFractionDigits: 0, maximumFractionDigits: 2 })}`
}
function usd(value: number) {
  return `$${value.toLocaleString('en-US', { maximumFractionDigits: 2 })}`
}
function count(value: number | null | undefined) {
  return value === null || value === undefined ? '—' : value.toLocaleString('zh-CN')
}
function purchase(day: OpsDay) {
  return day.purchaseUsd > 0 ? `${yuan(day.purchaseCny)} + ${usd(day.purchaseUsd)}` : yuan(day.purchaseCny)
}
function profitTone(value: number | null) {
  if (value === null)
    return 'text-cp-text-tertiary'
  return value >= 0 ? 'text-cp-success-text' : 'text-cp-error-text'
}

const cards = computed(() => {
  const day = today.value
  if (!day)
    return []
  return [
    { label: '今日新增号', value: count(day.newAccounts), hint: `买入 ${day.purchasedAccounts} 个` },
    { label: '今日买入成本', value: purchase(day), hint: '当天跑完，不摊销' },
    { label: 'Codex 经 CPR（调整后）', value: yuan(day.sub2api?.codexViaCpr.adjusted), hint: `标准价 ${yuan(day.sub2api?.codexViaCpr.standard)}` },
    { label: '今日毛利', value: yuan(day.grossProfit), hint: 'Codex 经 CPR − 买入成本', tone: profitTone(day.grossProfit) },
    { label: '全站消费（调整后）', value: yuan(day.sub2api?.adjusted), hint: `实际扣费 ${yuan(day.sub2api?.actual)}` },
    { label: '今日收款', value: yuan(day.sub2api?.payments), hint: `活跃用户 ${count(day.sub2api?.activeUsers)}` },
  ]
})

const { palette } = useChartPalette()
const option = computed<EChartsOption>(() => {
  const colors = palette.value
  const points = ascending.value
  const money = (value: unknown) => typeof value === 'number' ? yuan(value) : '—'
  return {
    animation: false,
    textStyle: { fontFamily: 'Inter Variable, Inter, sans-serif' },
    grid: { left: 8, right: 12, top: 40, bottom: 0, containLabel: true },
    tooltip: { trigger: 'axis', ...chartTooltipStyle(colors, { axisPointer: true, confine: true }), renderMode: 'richText', valueFormatter: money },
    legend: { top: 0, right: 0, icon: 'circle', itemWidth: 7, itemHeight: 7, textStyle: { color: colors.textSecondary, fontSize: 11 } },
    xAxis: {
      type: 'category',
      data: points.map(point => point.day.slice(5)),
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: { color: colors.textMuted, fontSize: 10, hideOverlap: true },
    },
    yAxis: { type: 'value', axisLabel: { color: colors.textMuted, fontSize: 10, formatter: (value: number) => `¥${value}` }, splitLine: { lineStyle: { color: colors.grid, type: 'dashed' } } },
    series: [
      { name: '买入成本', type: 'bar', barMaxWidth: 14, itemStyle: { color: colors.warning, borderRadius: 3 }, data: points.map(point => point.purchaseCny) },
      { name: 'Codex 经 CPR', type: 'bar', barMaxWidth: 14, itemStyle: { color: colors.info, borderRadius: 3 }, data: points.map(point => point.sub2api?.codexViaCpr.adjusted ?? null) },
      { name: '毛利', type: 'line', smooth: 0.2, symbolSize: 5, lineStyle: { color: colors.success, width: 2 }, itemStyle: { color: colors.success }, data: points.map(point => point.grossProfit) },
      { name: '收款', type: 'line', smooth: 0.2, symbolSize: 5, lineStyle: { color: colors.reasoning, width: 2, type: 'dashed' }, itemStyle: { color: colors.reasoning }, data: points.map(point => point.sub2api?.payments ?? null) },
    ],
  }
})
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="经营日报" description="按北京时间自然日汇总号池投入、sub2api 消费与收款（1 元 = 1 刀，英雄套餐 ÷5）">
      <template #actions>
        <BaseSelect v-model="range" :options="rangeOptions" class="w-34" @update:model-value="load" />
        <BaseButton :loading="loading" @click="load">
          <template #icon>
            <RefreshCw class="size-4" />
          </template>
          刷新
        </BaseButton>
      </template>
    </BasePageHeader>

    <p v-if="error" role="alert" class="mt-4 mb-0 text-cp-sm text-cp-error">
      {{ error }}
    </p>
    <p v-if="report && !report.sub2apiConfigured" class="mt-4 mb-0 text-cp-sm text-cp-warning-text">
      未配置 sub2api 只读数据源（store.ops_report.sub2api_database_url），只显示 CPR 自身数据。
    </p>

    <BaseEmpty
      v-if="report && !days.length"
      title="还没有日报数据"
      description="后台任务启动后几分钟内生成今天的数据，并逐步回填最近 30 天"
      class="mt-5"
    />

    <template v-if="days.length">
      <div class="mt-5 grid grid-cols-2 gap-3 md:grid-cols-3 xl:grid-cols-6">
        <BaseCard v-for="card in cards" :key="card.label">
          <div class="text-cp-xs text-cp-text-tertiary">
            {{ card.label }}
          </div>
          <div class="mt-2 text-2xl font-heavy tabular-nums" :class="card.tone ?? 'text-cp-text'">
            {{ card.value }}
          </div>
          <div class="mt-1 text-cp-xs text-cp-text-quaternary">
            {{ card.hint }}
          </div>
        </BaseCard>
      </div>

      <BaseCard class="mt-5" title="趋势" :description="today ? `更新于 ${formatDateTime(today.refreshedAt)}，每 10 分钟刷新` : undefined">
        <BaseChart :option="option" :height="300" />
      </BaseCard>

      <BaseCard class="mt-5" padding="none" title="每日明细">
        <div class="overflow-x-auto">
          <table class="w-full min-w-[1100px] border-collapse text-cp-sm tabular-nums">
            <thead>
              <tr class="border-b border-cp-border-secondary text-left text-cp-xs text-cp-text-tertiary">
                <th class="px-4 py-2.5 font-medium">
                  日期
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  新增号
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  买入成本
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  CPR 请求（失败）
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  CPR 官方价
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  全站 标准 / 实扣 / 调整后
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  Codex 调整后
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  Codex 经 CPR
                </th>
                <th class="px-3 py-2.5 text-right font-medium">
                  毛利
                </th>
                <th class="px-4 py-2.5 text-right font-medium">
                  收款
                </th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="day in days" :key="day.day" class="border-b border-cp-border-secondary last:border-b-0">
                <td class="px-4 py-2.5 whitespace-nowrap">
                  <span class="font-emphasis text-cp-text">{{ day.day }}</span>
                  <span v-if="!day.finalized" class="ml-1.5 text-[10px] text-cp-text-quaternary">进行中</span>
                </td>
                <td class="px-3 py-2.5 text-right">
                  {{ count(day.newAccounts) }}
                </td>
                <td class="px-3 py-2.5 text-right whitespace-nowrap">
                  {{ purchase(day) }}
                </td>
                <td class="px-3 py-2.5 text-right whitespace-nowrap">
                  {{ count(day.cpr.requests) }}
                  <span class="text-cp-text-quaternary">（{{ count(day.cpr.failedRequests) }}）</span>
                </td>
                <td class="px-3 py-2.5 text-right">
                  {{ usd(day.cpr.officialUsd) }}
                </td>
                <td class="px-3 py-2.5 text-right whitespace-nowrap text-cp-text-secondary">
                  <template v-if="day.sub2api">
                    {{ yuan(day.sub2api.standard) }} / {{ yuan(day.sub2api.actual) }} /
                    <span class="font-emphasis text-cp-text">{{ yuan(day.sub2api.adjusted) }}</span>
                  </template>
                  <template v-else>
                    —
                  </template>
                </td>
                <td class="px-3 py-2.5 text-right">
                  {{ yuan(day.sub2api?.codex.adjusted) }}
                </td>
                <td class="px-3 py-2.5 text-right font-emphasis">
                  {{ yuan(day.sub2api?.codexViaCpr.adjusted) }}
                </td>
                <td class="px-3 py-2.5 text-right font-heavy" :class="profitTone(day.grossProfit)">
                  {{ yuan(day.grossProfit) }}
                </td>
                <td class="px-4 py-2.5 text-right">
                  {{ yuan(day.sub2api?.payments) }}
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </BaseCard>
    </template>
  </div>
</template>
