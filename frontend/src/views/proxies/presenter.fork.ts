// fork: proxy-quality — 代理质量检测与出口地理的展示文案；复制地址的格式列表。
import type { OutboundProxyExitGeo, ProxyQualityItemStatus, ProxyQualityStatus } from '@/api'

export function exitGeoLabel(geo: OutboundProxyExitGeo | null | undefined): string {
  if (!geo)
    return ''
  return [geo.country, geo.city ?? geo.region].filter(Boolean).join(' · ')
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
