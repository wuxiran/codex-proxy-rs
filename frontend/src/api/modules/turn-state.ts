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
  /** WS 保活：为已绑定 state 的账号预建并挂住满血 WebSocket，业务复用。 */
  warmPool: TurnStateWarmPoolSettings
}

export interface TurnStateWarmPoolSettings {
  /** 默认开；只对已开启「固定自身 state」的账号(导入的新号)生效，不碰存量号。 */
  enabled: boolean
  /** 业务请求是否可领养保活连接。 */
  businessReuse: boolean
  /** 每个账号挂几条满血连接。 */
  connectionsPerAccount: number
  /** 预热的模型；空 = 用探针模型。 */
  models: string[]
  /** 一条连接最多挂多久(秒)，<上游 55min。 */
  maxAgeSeconds: number
  /** 复探间隔(秒)：低频验连接还满不满血。 */
  reprobeSeconds: number
  /** 是否跑 canary 探针(糖果题)。 */
  probe: boolean
  /** 探针题正文；空 = 内置糖果题。 */
  probePrompt: string
  /** 满血判据：答案以此开头(如 21)。 */
  probeExpect: string
  /** 探针模型；空 = models 首个。 */
  probeModel: string
  /** 探针 effort。 */
  probeEffort: 'low' | 'medium' | 'high' | 'xhigh'
  /** 单次探针整次上限(秒)。 */
  probeTimeoutSeconds: number
  /** 探到降智时最多再换几个节点重试。 */
  probeRetries: number
  /** 降智/失败后该账号冷却多久(秒)。 */
  cooldownSeconds: number
  /** 进程内保活连接总数上限。 */
  maxTotalConnections: number
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
