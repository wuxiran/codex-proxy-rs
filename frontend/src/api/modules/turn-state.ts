import type { RequestOptions } from '../request'
import request from '../request'

export type TurnStateInjectMode = 'always' | 'replace-only'

/** turn-state 运行设置；整体替换，字段全部必填。 */
export interface TurnStateSettings {
  ttlSeconds: number
  injectMode: TurnStateInjectMode
  dryRun: boolean
  logDecisions: boolean
  /** 可入库的模板长度；空 = 退回 ≥200 字节下限规则。 */
  templateLengths: number[]
  /** 受限/降级档长度；永不入库，replace-only 只替换这些。 */
  degradedLengths: number[]
  /** 云端打票：账号缺票时向 relay 铸票并钉住路由 cookie 对。 */
  cloudMint: TurnStateCloudMintSettings
}

export type TurnStateMintMode = 'native' | 'relay'

export interface TurnStateCloudMintSettings {
  enabled: boolean
  /** native：cpr 经账号代理直打上游；relay：交给 deploy/cloud-mint 的 relay。 */
  mode: TurnStateMintMode
  /** 只观测不注入：照常打票、记录，但不把 pair/票写进业务请求。 */
  observeOnly: boolean
  relayUrl: string
  /** 读取时为 `<set>` 占位或空；提交 `<set>` 表示沿用已保存的密钥。 */
  relayKey: string
  proxyUrl: string
  gateway: string
  ticketLen: number
  ticketTtlSeconds: number
  models: string[]
  transport: 'sse' | 'websocket'
  cooldownSeconds: number
  /** 原生打票每个模型最多发几次。 */
  maxAttempts: number
}

export type TurnStateSource = 'hunt' | 'passive' | 'renewal' | 'mint'
export type TurnStateIssuedAtSource = 'fernet' | 'captured'
export type TurnStateLengthClass = 'normal' | 'degraded' | 'unknown'
export type TurnStateDecision = 'inject' | 'substitute' | 'pass' | 'skip' | 'harvest'

/** 一个桶里的模板摘要；没有票值。时间均为 Unix 秒。 */
export interface TurnStateBucket {
  account: string
  model: string
  scope: 'account' | 'client'
  len: number
  issuedAt: number
  issuedAtSource: TurnStateIssuedAtSource
  capturedAt: number
  expiresAt: number
  source: TurnStateSource
  egress: string | null
  hits: number
  /** 票所属网关节点（云端打票的票才有）。 */
  gateway: string | null
}

export interface TurnStateBucketsResponse {
  now: number
  buckets: TurnStateBucket[]
}

export interface TurnStateBucketTally {
  normal: number
  degraded: number
  unknown: number
  silent: number
  injectedTotal: number
  injectedSilent: number
  injectedDegraded: number
  injectedNormal: number
  substituted: number
  lastSeenAt: number
  lastIssuedLen: number | null
  lastIssuedAt: number
  lengths: Record<string, number>
}

export interface TurnStateHourTally {
  hour: number
  normal: number
  degraded: number
  unknown: number
  silent: number
  injected: number
}

export interface TurnStateObservationEvent {
  id: string
  at: number
  account: string
  model: string
  decision: TurnStateDecision
  class: TurnStateLengthClass | null
  len: number | null
  injected: boolean
}

export interface TurnStateObservations {
  now: number
  version: number
  updatedAt: number
  /** 键为 `<账号>/<模型>`。 */
  buckets: Record<string, TurnStateBucketTally>
  hourly: TurnStateHourTally[]
  histogram: Record<string, number>
  events: TurnStateObservationEvent[]
}

export function getTurnStateSettings(options: RequestOptions = {}) {
  return request<TurnStateSettings>({
    url: '/api/admin/turn-state/settings',
    method: 'GET',
    ...options,
  })
}

export function updateTurnStateSettings(data: TurnStateSettings) {
  return request<TurnStateSettings>({
    url: '/api/admin/turn-state/settings/update',
    method: 'POST',
    data,
  })
}

export function getTurnStateObservations(options: RequestOptions = {}) {
  return request<TurnStateObservations>({
    url: '/api/admin/turn-state/observations',
    method: 'GET',
    ...options,
  })
}

export function getTurnStateBuckets(params: { account?: string, model?: string } = {}, options: RequestOptions = {}) {
  return request<TurnStateBucketsResponse>({
    url: '/api/admin/turn-state/buckets',
    method: 'GET',
    params,
    ...options,
  })
}

export function clearTurnStateBucket(data: { account: string, model?: string }) {
  return request<{ cleared: number }>({
    url: '/api/admin/turn-state/buckets/clear',
    method: 'POST',
    data,
  })
}
