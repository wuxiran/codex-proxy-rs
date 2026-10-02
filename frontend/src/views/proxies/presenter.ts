import type { OutboundProxyExitGeo, OutboundProxyTest, ProxyQualityItemStatus, ProxyQualityStatus } from '@/api'

export function exitGeoLabel(geo: OutboundProxyExitGeo | null | undefined): string {
  if (!geo)
    return ''
  return [geo.country, geo.city ?? geo.region].filter(Boolean).join(' · ')
}

/** 耗时包含代理握手与 TLS：200ms 内算快，800ms 以上对流式首字已有明显影响。 */
export function latencyToneClass(test: OutboundProxyTest): string {
  if (test.latencyMs < 200)
    return 'text-cp-success-text'
  return test.latencyMs < 800 ? 'text-cp-warning-text' : 'text-cp-error-text'
}

const qualityStatusPresentation: Record<ProxyQualityStatus, { label: string, badge: string }> = {
  healthy: { label: '优质', badge: 'bg-cp-success-container text-cp-success-on-container' },
  warn: { label: '告警', badge: 'bg-cp-warning-container text-cp-warning-on-container' },
  challenge: { label: '挑战', badge: 'bg-cp-error-container text-cp-error-on-container' },
  failed: { label: '异常', badge: 'bg-cp-error-container text-cp-error-on-container' },
}

export function qualityStatus(status: ProxyQualityStatus) {
  return qualityStatusPresentation[status] ?? qualityStatusPresentation.failed
}

const qualityItemPresentation: Record<ProxyQualityItemStatus, { label: string, badge: string }> = {
  pass: { label: '通过', badge: 'bg-cp-success-container text-cp-success-on-container' },
  warn: { label: '告警', badge: 'bg-cp-warning-container text-cp-warning-on-container' },
  challenge: { label: '挑战', badge: 'bg-cp-error-container text-cp-error-on-container' },
  fail: { label: '失败', badge: 'bg-cp-error-container text-cp-error-on-container' },
}

export function qualityItemStatus(status: ProxyQualityItemStatus) {
  return qualityItemPresentation[status] ?? qualityItemPresentation.fail
}

const qualityTargetLabels: Record<string, string> = {
  base_connectivity: '基础连通性',
  chatgpt: 'ChatGPT（Codex 后端）',
  openai_auth: 'OpenAI 登录与令牌',
  openai_api: 'OpenAI API',
  xai: 'xAI API',
}

export function qualityTargetLabel(target: string): string {
  return qualityTargetLabels[target] ?? target
}

/** 后端只下发脱敏端点，因此可复制的格式里永远不含账号密码。 */
export function endpointCopyFormats(endpoint: string): Array<{ label: string, value: string }> {
  const full = endpoint.replace(/\/$/, '')
  const hostPort = full.replace(/^[a-z0-9]+:\/\//i, '')
  return [
    { label: full, value: full },
    { label: hostPort, value: hostPort },
  ]
}

export interface ParsedProxyLines {
  valid: string[]
  invalid: number
  duplicate: number
}

const PROXY_LINE = /^(?:https?|socks5h?):\/\/(?:[^\s:@/]+:[^\s@/]+@)?(?:\[[0-9a-f:.]+\]|[^\s:@/[\]]+):(\d{1,5})\/?$/i

/** 与后端解析规则对齐的预检；最终以后端逐条校验为准，这里只为即时反馈。 */
export function parseProxyLines(input: string): ParsedProxyLines {
  const result: ParsedProxyLines = { valid: [], invalid: 0, duplicate: 0 }
  const seen = new Set<string>()
  for (const raw of input.split(/\r?\n/)) {
    const line = raw.trim()
    if (!line)
      continue
    const port = Number(PROXY_LINE.exec(line)?.[1])
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      result.invalid += 1
      continue
    }
    const key = line.replace(/\/$/, '').toLowerCase()
    if (seen.has(key)) {
      result.duplicate += 1
      continue
    }
    seen.add(key)
    result.valid.push(line)
  }
  return result
}
