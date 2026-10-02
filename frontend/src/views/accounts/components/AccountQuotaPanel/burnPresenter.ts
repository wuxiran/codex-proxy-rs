import type { EChartsOption, LineSeriesOption } from 'echarts'
import type { AccountQuotaForecast } from '@/api'
import dayjs from 'dayjs'
import { chartTooltipStyle } from '@/components/charts/tooltip'
import { parseTimestamp } from '@/utils/date'

interface BurnChartColors {
  surface: string
  textPrimary: string
  textMuted: string
  pointer: string
  grid: string
  info: string
  warning: string
  danger: string
}

type BurnPoint = [time: number, usedPercent: number]

const HOUR_MS = 3_600_000

export function burnCurvePoints(forecast: AccountQuotaForecast): BurnPoint[] {
  // 滚动发布期间旧后端尚无 `curve`；按空曲线处理，面板退回到说明文案。
  return (forecast.curve ?? []).flatMap((point): BurnPoint[] => {
    const time = parseTimestamp(point.observedAt)
    return time === null || !Number.isFinite(point.usedPercent) ? [] : [[time, point.usedPercent]]
  })
}

/** 虚线只延伸后端给出的同一速率；没有速率或已耗尽时不画，避免前端另造一套预测。 */
function projectionPoints(forecast: AccountQuotaForecast, last: BurnPoint, resetAt: number): BurnPoint[] {
  const exhaustion = forecast.exhaustion
  if (exhaustion?.kind === 'at') {
    const at = parseTimestamp(exhaustion.at ?? '')
    return at !== null && at > last[0] ? [last, [at, 100]] : []
  }
  const rate = forecast.burnPercentPerHour
  if (exhaustion?.kind !== 'afterReset' || rate === null || resetAt <= last[0])
    return []
  return [last, [resetAt, Math.min(100, last[1] + rate * (resetAt - last[0]) / HOUR_MS)]]
}

function burnTone(usedPercent: number, colors: BurnChartColors) {
  if (usedPercent >= 90)
    return colors.danger
  return usedPercent >= 70 ? colors.warning : colors.info
}

export function burnChartOption(
  forecast: AccountQuotaForecast,
  colors: BurnChartColors,
  reduceMotion: boolean,
): EChartsOption | null {
  const points = burnCurvePoints(forecast)
  const last = points.at(-1)
  const start = parseTimestamp(forecast.windowStartAt ?? '')
  const resetAt = parseTimestamp(forecast.source?.resetAt ?? '')
  if (!last || start === null || resetAt === null || resetAt <= start)
    return null

  const tone = burnTone(last[1], colors)
  const projection = projectionPoints(forecast, last, resetAt)
  const observed: LineSeriesOption = {
    name: '已用',
    type: 'line',
    data: points,
    // 已用比例单调上升，平滑会在两点之间画出不存在的回落。
    smooth: false,
    showSymbol: points.length <= 1,
    symbolSize: 5,
    lineStyle: { color: tone, width: 2 },
    itemStyle: { color: tone },
    areaStyle: { color: tone, opacity: 0.12 },
  }
  const projected: LineSeriesOption = {
    name: '预计',
    type: 'line',
    data: projection,
    showSymbol: false,
    lineStyle: { color: tone, width: 1.5, type: 'dashed', opacity: 0.75 },
    itemStyle: { color: tone },
    // 起点与实线末点重合，悬停时不重复报同一个值。
    tooltip: { show: false },
    silent: true,
  }

  return {
    animation: !reduceMotion,
    animationDuration: 420,
    textStyle: { fontFamily: 'Inter Variable, Inter, sans-serif' },
    grid: { left: 4, right: 10, top: 14, bottom: 0, containLabel: true },
    tooltip: {
      trigger: 'axis',
      ...chartTooltipStyle(colors, { axisPointer: true, confine: true }),
      renderMode: 'richText',
      formatter: (params) => {
        const row = (Array.isArray(params) ? params : [params]).find(item => item.seriesName === '已用')
        const value = Array.isArray(row?.value) ? row.value : null
        if (!value)
          return ''
        return `${dayjs(Number(value[0])).format('MM-DD HH:mm')}\n已用 ${Number(value[1]).toFixed(1)}%`
      },
    },
    xAxis: {
      type: 'time',
      min: start,
      max: resetAt,
      axisLine: { show: false },
      axisTick: { show: false },
      splitLine: { show: false },
      axisLabel: {
        color: colors.textMuted,
        fontSize: 10,
        hideOverlap: true,
        formatter: (value: number) => dayjs(value).format('MM-DD'),
      },
    },
    yAxis: {
      type: 'value',
      min: 0,
      max: 100,
      interval: 50,
      axisLabel: { color: colors.textMuted, fontSize: 10, formatter: '{value}%' },
      splitLine: { lineStyle: { color: colors.grid, type: 'dashed' } },
    },
    series: projection.length > 0 ? [observed, projected] : [observed],
  }
}

/** 面向未来的时长；`formatRelativeTime` 只描述过去，不能复用。 */
export function formatDurationUntil(target: string | null, now: number): string | null {
  const time = parseTimestamp(target ?? '')
  if (time === null)
    return null
  const minutes = Math.floor((time - now) / 60_000)
  if (minutes < 1)
    return '即将'
  if (minutes < 60)
    return `约 ${minutes} 分钟后`
  const hours = Math.floor(minutes / 60)
  if (hours < 48)
    return `约 ${hours} 小时后`
  return `约 ${Math.floor(hours / 24)} 天后`
}
