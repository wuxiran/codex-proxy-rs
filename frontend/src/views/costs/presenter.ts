import type { BarSeriesOption, EChartsOption, LineSeriesOption } from 'echarts'
import type { AccountCostRow, CostDay } from '@/api'
import { chartTooltipStyle } from '@/components/charts/tooltip'

export function formatMoney(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value))
    return '—'
  return value.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })
}

/** 每 1 刀的成本，三位小数足以区分 0.093 与 0.107 这类差异。 */
export function formatCostPerUsd(value: number | null | undefined): string {
  return value == null || !Number.isFinite(value) ? '—' : value.toFixed(3)
}

export type AccountCostState = 'retired' | 'deleted' | 'finished' | 'running'

/** 系统只提示、不替管理员做下线决定：号可能只是暂时掉线。 */
export function accountCostState(row: AccountCostRow): AccountCostState {
  if (row.retiredAt)
    return 'retired'
  if (!row.accountExists)
    return 'deleted'
  return row.quotaExhausted || row.enabled === false || row.credentialReady === false ? 'finished' : 'running'
}

export const accountCostStateLabels: Record<AccountCostState, { label: string, badge: string }> = {
  running: { label: '在跑', badge: 'bg-cp-success-container text-cp-success-on-container' },
  finished: { label: '可下线', badge: 'bg-cp-warning-container text-cp-warning-on-container' },
  retired: { label: '已下线', badge: 'bg-cp-fill-secondary text-cp-text-secondary' },
  deleted: { label: '号已删除', badge: 'bg-cp-fill-secondary text-cp-text-secondary' },
}

interface CostChartColors {
  surface: string
  textPrimary: string
  textSecondary: string
  textMuted: string
  pointer: string
  grid: string
  info: string
  success: string
  warning: string
}

export function costChartOption(days: CostDay[], colors: CostChartColors, reduceMotion: boolean): EChartsOption {
  const bar = (name: string, color: string, data: number[]): BarSeriesOption => ({
    name,
    type: 'bar',
    data,
    barMaxWidth: 18,
    itemStyle: { color, borderRadius: [3, 3, 0, 0] },
  })
  const ratio: LineSeriesOption = {
    name: '每 1 刀成本',
    type: 'line',
    yAxisIndex: 1,
    // 没有跑出的日期没有比值；断开而不是画到 0。
    data: days.map(day => day.costPerUsd),
    connectNulls: false,
    showSymbol: days.length <= 31,
    symbolSize: 5,
    lineStyle: { color: colors.warning, width: 2 },
    itemStyle: { color: colors.warning },
  }
  return {
    animation: !reduceMotion,
    animationDuration: 420,
    textStyle: { fontFamily: 'Inter Variable, Inter, sans-serif' },
    grid: { left: 8, right: 8, top: 40, bottom: 0, containLabel: true },
    legend: { top: 0, right: 0, type: 'plain', icon: 'circle', itemWidth: 7, itemHeight: 7, textStyle: { color: colors.textSecondary, fontSize: 11 } },
    tooltip: {
      trigger: 'axis',
      ...chartTooltipStyle(colors, { axisPointer: true, confine: true }),
      renderMode: 'richText',
      valueFormatter: value => typeof value === 'number' ? (value < 10 ? value.toFixed(3) : formatMoney(value)) : '—',
    },
    xAxis: {
      type: 'category',
      data: days.map(day => day.day.slice(5)),
      axisLine: { show: false },
      axisTick: { show: false },
      axisLabel: { color: colors.textMuted, fontSize: 10, hideOverlap: true },
    },
    yAxis: [
      { type: 'value', min: 0, axisLabel: { color: colors.textMuted, fontSize: 10 }, splitLine: { lineStyle: { color: colors.grid, type: 'dashed' } } },
      { type: 'value', min: 0, axisLabel: { color: colors.textMuted, fontSize: 10, formatter: (value: number) => value.toFixed(2) }, splitLine: { show: false } },
    ],
    series: [
      bar('投入', colors.info, days.map(day => day.spend)),
      bar('跑出（USD）', colors.success, days.map(day => day.usageUsd)),
      ratio,
    ],
  }
}
