// fork: 时间格式化。上游 v3.19 起移除了 dayjs 与 formatDateTime / parseTimestamp（展示文案改由后端下发 *Display 字段），
// fork 自有页面（票据、免登录导入、经营日报、turn-state、消耗曲线）仍需在前端按浏览器本地时区格式化，统一放在这里。

export type DateInput = string | number | Date | null | undefined

function toDate(value: DateInput): Date | null {
  if (value == null || value === '')
    return null
  // 后端偶有 `YYYY-MM-DD HH:mm:ss` 形式；补成 ISO 再解析，避免 Safari 判定为 Invalid Date。
  const date = typeof value === 'string' ? new Date(value.replace(' ', 'T')) : new Date(value)
  return Number.isNaN(date.getTime()) ? null : date
}

function pad(value: number) {
  return String(value).padStart(2, '0')
}

function parts(date: Date) {
  return {
    year: date.getFullYear(),
    month: pad(date.getMonth() + 1),
    day: pad(date.getDate()),
    hour: pad(date.getHours()),
    minute: pad(date.getMinutes()),
    second: pad(date.getSeconds()),
  }
}

/** 解析成毫秒时间戳；无效输入返回 null。 */
export function parseTimestamp(value: DateInput): number | null {
  return toDate(value)?.getTime() ?? null
}

/** `YYYY-MM-DD HH:mm:ss`（本地时区）；无效输入返回 fallback。 */
export function formatDateTime(value: DateInput = new Date(), fallback = '—'): string {
  const date = toDate(value)
  if (!date)
    return fallback
  const { year, month, day, hour, minute, second } = parts(date)
  return `${year}-${month}-${day} ${hour}:${minute}:${second}`
}

/** `<input type="datetime-local">` 需要的 `YYYY-MM-DDTHH:mm`；无效输入返回空串。 */
export function formatDateTimeLocalInput(value: DateInput): string {
  const date = toDate(value)
  if (!date)
    return ''
  const { year, month, day, hour, minute } = parts(date)
  return `${year}-${month}-${day}T${hour}:${minute}`
}

/** 图表坐标轴用的 `MM-DD`。 */
export function formatMonthDay(value: DateInput): string {
  const date = toDate(value)
  if (!date)
    return ''
  const { month, day } = parts(date)
  return `${month}-${day}`
}

/** 图表提示用的 `MM-DD HH:mm`。 */
export function formatMonthDayTime(value: DateInput): string {
  const date = toDate(value)
  if (!date)
    return ''
  const { month, day, hour, minute } = parts(date)
  return `${month}-${day} ${hour}:${minute}`
}
