import type { ConfigType } from 'dayjs'
import dayjs from 'dayjs'
import timezone from 'dayjs/plugin/timezone'
import utc from 'dayjs/plugin/utc'

dayjs.extend(utc)
dayjs.extend(timezone)

const DATE_TIME_FORMAT = 'YYYY-MM-DD HH:mm:ss'
const TIME_FORMAT = 'HH:mm:ss'

export function formatDateTime(value: ConfigType = new Date(), fallback = '—', timeZone?: string): string {
  const timestamp = normalizedDate(value)
  if (!timestamp.isValid())
    return fallback
  // 显式指定时区只影响展示，现有调用继续使用浏览器本地时区。
  return (timeZone ? timestamp.tz(timeZone) : timestamp).format(DATE_TIME_FORMAT)
}

export function formatTime(value: ConfigType = new Date(), fallback = '—'): string {
  const timestamp = normalizedDate(value)
  return timestamp.isValid() ? timestamp.format(TIME_FORMAT) : fallback
}

export function parseTimestamp(value: ConfigType): number | null {
  const timestamp = normalizedDate(value)
  return timestamp.isValid() ? timestamp.valueOf() : null
}

export function formatRelativeTime(
  value: ConfigType,
  now: ConfigType = new Date(),
): string {
  const timestamp = normalizedDate(value)
  if (!timestamp.isValid())
    return '—'

  const elapsedSeconds = Math.max(0, dayjs(now).diff(timestamp, 'second'))
  if (elapsedSeconds < 60)
    return '刚刚'

  const elapsedMinutes = Math.floor(elapsedSeconds / 60)
  if (elapsedMinutes < 60)
    return `${elapsedMinutes} 分钟前`

  const elapsedHours = Math.floor(elapsedMinutes / 60)
  if (elapsedHours < 24)
    return `${elapsedHours} 小时前`

  return `${Math.floor(elapsedHours / 24)} 天前`
}

function normalizedDate(value: ConfigType) {
  return dayjs(typeof value === 'string' ? value.replace(' ', 'T') : value)
}

const integerFormatter = new Intl.NumberFormat('zh-CN')

const localizedCompactFormatter = new Intl.NumberFormat('zh-CN', {
  notation: 'compact',
  maximumFractionDigits: 1,
})

const metricUnits = [
  ['P', 1_000_000_000_000_000],
  ['T', 1_000_000_000_000],
  ['B', 1_000_000_000],
  ['M', 1_000_000],
  ['K', 1_000],
] as const

export function formatInteger(value: number) {
  return integerFormatter.format(value)
}

export function formatLocalizedCompactNumber(value: number) {
  return localizedCompactFormatter.format(Number.isFinite(value) ? value : 0)
}

export function formatCompactNumber(value: number) {
  const normalized = Math.max(0, Math.round(value))
  if (normalized < 1_000)
    return formatInteger(normalized)

  for (const [unit, threshold] of metricUnits) {
    if (normalized < threshold)
      continue

    const scaled = normalized / threshold
    const rounded = scaled >= 10 ? scaled.toFixed(1) : scaled.toFixed(2)
    return `${rounded.replace(/\.?0+$/, '')}${unit}`
  }

  return formatInteger(normalized)
}
