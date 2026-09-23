import type { RequestOptions } from '../request'
import type { AccountGroupRef } from './account-groups'
import { API_BASE_URL } from '../constants'
import request from '../request'

export type AccountStatus
  = 'normal' | 'quota_exhausted' | 'rate_limited' | 'disabled' | 'error'

export type AccountErrorReason
  = 'account_unverified'
    | 'access_token_expired'
    | 'credential_expired'
    | 'credential_invalid'
    | 'account_banned'

export interface AccountQuotaWindow {
  key: string
  group: string
  limitId: string | null
  limitName: string | null
  role: 'primary' | 'secondary' | 'monthly' | null
  windowSeconds: number | null
  labelDisplay: string
  windowLabelDisplay: string
  usedPercent: number | null
  usedPercentDisplay: string
  limitReached: boolean
  localUsage?: unknown
  resetAtDisplay: string
}

export interface AccountQuota {
  refreshedAtDisplay: string
  limitReached: boolean
  // 429 临时限流（Redis 冷却）到期时间；非限流中为 null。
  rateLimitedUntil: string | null
  rateLimitReason: 'upstream_rate_limit' | 'capacity_freeze' | null
  recoveryProbeRequired: boolean
  windows: AccountQuotaWindow[]
}

export interface AccountCurrencyCost {
  currency: string
  estimatedAmount: string
  estimatedAmountDisplay: string
}

export interface AccountBilling {
  modelPriceAmountUsd: string | null
  modelPriceAmountUsdDisplay: string
  upstreamCostAmountUsd: string | null
  upstreamCostAmountUsdDisplay: string
  differenceAmountUsd: string | null
  differenceAmountUsdDisplay: string
  modelPriceCount: number
  upstreamCostCount: number
}

export interface AccountModelUsage {
  key: string
  requestedModelId: string | null
  upstreamModelId: string | null
  responseModel: string | null
  billingModel: string | null
  mismatch: boolean
  billing: AccountBilling
  model: string
  requestCount: number
  requestCountDisplay: string
  successRate: number | null
  successRateDisplay: string
  inputTokens: number | null
  inputTokensDisplay: string
  outputTokens: number | null
  outputTokensDisplay: string
  cachedTokens: number | null
  cachedTokensDisplay: string
  imageInputTokens: number | null
  imageInputTokensDisplay: string
  imageOutputTokens: number | null
  imageOutputTokensDisplay: string
  imageRequestCount: number
  imageRequestCountDisplay: string
  imageRequestFailedCount: number
  imageRequestFailedCountDisplay: string
  totalTokens: number | null
  totalTokensDisplay: string
  billingAmountUsd: string | null
  billingAmountUsdDisplay: string
  costEstimateStatus: string
  knownCostCount: number
  partialCostCount: number
  unknownCostCount: number
  costs: AccountCurrencyCost[]
  lastUsedAt: string
  lastUsedAtDisplay: string
}

export interface AccountUsage {
  billing: AccountBilling
  /** 按额度窗口已用比例外推的整窗口额度（按模型价格计）；无窗口数据时为空。 */
  estimatedQuotaUsdDisplay: string | null
  windowLabelDisplay: string
  requestCount: number | null
  requestCountDisplay: string
  inputTokens: number | null
  inputTokensDisplay: string
  outputTokens: number | null
  outputTokensDisplay: string
  cachedTokens: number | null
  cachedTokensDisplay: string
  reasoningTokens: number | null
  reasoningTokensDisplay: string
  imageInputTokens: number | null
  imageInputTokensDisplay: string
  imageOutputTokens: number | null
  imageOutputTokensDisplay: string
  imageRequestCount: number | null
  imageRequestCountDisplay: string
  imageRequestFailedCount: number | null
  imageRequestFailedCountDisplay: string
  totalTokens: number | null
  totalTokensDisplay: string
  createdTokens: number | null
  createdTokensDisplay: string
  readTokens: number | null
  readTokensDisplay: string
  lastUsedAt: string | null
  lastUsedAtDisplay: string
  costEstimateStatus: string
  knownCostCount: number | null
  partialCostCount: number | null
  unknownCostCount: number | null
  costs: AccountCurrencyCost[]
  models: AccountModelUsage[]
}

export interface AccountModelAccess {
  mode: 'all' | 'allowlist' | 'denylist'
  models: string[]
}

/** 账号成本、到期与票据状态；票据只回显打码邮箱。 */
export interface AccountTicket {
  purchaseAmount: string | null
  purchaseCurrency: 'CNY' | 'USD' | null
  purchaseDisplay: string | null
  purchasedAt: string | null
  expiresAt: string | null
  hasTicket: boolean
  ticketHint: string | null
  ticketUpdatedAt: string | null
  /** 自买入（或入库）起按模型价格计费的累计美元金额。 */
  spentUsd: string | null
  spentUsdDisplay: string | null
}

export interface AccountTicketUpdate {
  accountId: string
  purchaseAmount?: string | null
  purchaseCurrency?: 'CNY' | 'USD' | null
  purchasedAt?: string | null
  expiresAt?: string | null
  /** `邮箱----密码----2FA密钥`；不传保持原票据不变。 */
  ticket?: string
  clearTicket?: boolean
}

export interface Account {
  outboundProxyEndpoint: string | null
  id: string
  name: string
  notes: string | null
  provider: string
  resourceRef: string
  email: string | null
  accountId: string | null
  userId: string | null
  label: string | null
  planType: string | null
  planTypeDisplay: string
  authenticationKind: string
  hasRefreshToken: boolean
  status: AccountStatus
  errorReason: AccountErrorReason | null
  errorMessage: string | null
  enabled: boolean
  concurrencyLimit: number | null
  /** 实时并发：inFlight 为空表示实时数据不可用；limit 为空表示不限（未单独设置时为全局默认）。 */
  concurrency: { inFlight: number | null, limit: number | null }
  /** 最近 24 小时已结束的请求数与报错次数（失败 + 未完成，客户端取消不计）。 */
  recentErrors: { requestCount: number, errorCount: number }
  ticket: AccountTicket
  weight: number
  modelAccess: AccountModelAccess
  accessTokenExpiresAt: string | null
  accessTokenExpiresAtDisplay: string | null
  refreshTokenExpiresAt: string | null
  nextRefreshAt: string | null
  addedAt: string
  addedAtDisplay: string
  updatedAt: string
  updatedAtDisplay: string
  quota: AccountQuota
  usage: AccountUsage
  groups: AccountGroupRef[]
}

export interface AccountQuotaForecast {
  period: 'weekly' | 'monthly'
  targetDays: number
  extrapolated: boolean
  source: {
    label: string
    usedPercent: number | null
    usedPercentDisplay: string
    observedAt: string | null
    observedAtDisplay: string
    resetAt: string
    tokensDisplay: string
    usdDisplay: string
  } | null
  unavailableReason: string | null
  lowSample: boolean
  incompleteCost: boolean
  incompleteTokens: boolean
  estimatedTokens: number | null
  estimatedTokensDisplay: string
  estimatedUsd: number | null
  estimatedUsdDisplay: string
  remainingTokens: number | null
  remainingTokensDisplay: string
  remainingUsd: number | null
  remainingUsdDisplay: string
  windowStartAt: string | null
  curve: AccountQuotaCurvePoint[]
  burnPercentPerHour: number | null
  burnPercentPerHourDisplay: string
  exhaustion: AccountQuotaExhaustion | null
}

export interface AccountQuotaCurvePoint {
  observedAt: string
  usedPercent: number
}

export interface AccountQuotaExhaustion {
  kind: 'at' | 'afterReset' | 'reached'
  at: string | null
  atDisplay: string | null
}

export interface AccountQuotaForecastResponse {
  accountId: string
  generatedAt: string
  forecasts: AccountQuotaForecast[]
}

export interface AccountPageMeta {
  page: number
  pageSize: number
  total: number
  totalPages: number
}

export interface AccountSummary {
  total: number
  normal: number
  quotaExhausted: number
  rateLimited: number
  disabled: number
  error: number
}

export interface AccountListResponse {
  items: Account[]
  page: AccountPageMeta
  summary: AccountSummary
}

export interface AccountRefreshResponse {
  account: Account
  result?: string
  error?: string
}

export interface AccountQuotaResponse {
  account: Account
}

export interface AccountProfileStatisticsSummary {
  totalTextTokens: number | null
  peakTokens: number | null
  longestTaskDurationMs: number | null
  currentStreakDays: number | null
  longestStreakDays: number | null
}

export interface AccountProfileDailyUsage {
  date: string
  tokens: number
}

export interface AccountProfileInvocation {
  type: string
  pluginId: string | null
  pluginName: string | null
  skillId: string | null
  skillName: string | null
  usageCount: number | null
}

export interface AccountProfileActivityInsights {
  fastModePercent: number | null
  reasoningEffort: string | null
  reasoningEffortPercent: number | null
  skillsExplored: number | null
  totalSkillsUsed: number | null
  totalThreads: number | null
  invocations: AccountProfileInvocation[] | null
}

export interface AccountSubscription {
  startsAt: string | null
  expiresAt: string
  willRenew: boolean | null
  billingPeriod: string | null
  billingCurrency: string | null
  observedAt: string
}

export interface AccountProfileStatisticsResponse {
  displayName: string | null
  username: string | null
  imageUrl: string | null
  hasStatsError: boolean
  summary: AccountProfileStatisticsSummary
  dailyUsage: AccountProfileDailyUsage[] | null
  activityInsights: AccountProfileActivityInsights
}

export interface AccountPersonalInfoResponse {
  profile: AccountProfileStatisticsResponse | null
  profileError: string | null
  subscription: AccountSubscription | null
}

export interface AccountResetCredit {
  id: string
  status: string | null
  title: string | null
  expiresAt: string | null
  resetType: string | null
}

export interface AccountResetCreditsResponse {
  availableCount: number
  credits: AccountResetCredit[]
}

export interface AccountResetCreditResultResponse {
  code: string
  credit: AccountResetCredit | null
}

export interface AccountModelsResponse {
  models: Array<{ id: string, label: string }>
}

export interface AccountImportResponse {
  importedCount: number
  accountIds: string[]
}

export type ImportItemStatus = 'pending' | 'running' | 'succeeded' | 'failed' | 'unknown' | 'skipped'

export interface AccountImportTask {
  taskId: string
  createdAt: string
  finishedAt: string | null
  stopRequested: boolean
  total: number
  counts: Record<ImportItemStatus, number> & { importedAccounts: number }
}

export interface AccountImportTaskItem {
  index: number
  provider: string
  status: ImportItemStatus
  accountIds: string[]
  message: string | null
}

export interface AccountImportTaskDetail extends AccountImportTask {
  items: AccountImportTaskItem[]
}

export interface AccountOAuthCompleteResponse {
  accountId: string
}

export interface AccountUpdateResponse {
  accountId: string
  configRevision: number
}

export interface AccountBatchUpdateResponse {
  accountIds: string[]
  configRevision: number
}

export interface AccountDeletionResponse {
  deletedCount: number
  accountIds: string[]
}

export interface AccountOAuthStartResponse {
  flowId: string
  authorizationUrl: string
  expiresAt: string
}

// 请求参数类型：仅定义 API 边界的形状，调用方不依赖显式声明。
interface AccountListParams {
  page: number
  pageSize: number
  search?: string
  provider?: string
  status?: string
  groupId?: string
  sortBy?: string
  sortDirection?: string
}

interface AccountIdParam {
  accountId: string
}

interface AccountResetCreditConsumeParam extends AccountIdParam {
  creditId?: string
  redeemRequestId: string
}

interface AccountUpdateParam {
  outboundProxyUrl?: string
  outboundProxyId?: string
  accountId: string
  notes?: string
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  modelAccess?: AccountModelAccess
  groupIds: string[]
}

interface AccountBatchUpdateParam {
  outboundProxyUrl?: string
  outboundProxyId?: string
  accountIds: string[]
  enabled?: boolean
  concurrencyLimit?: number | null
  weight?: number
  modelAccess?: AccountModelAccess
  groupIds?: string[]
}

interface AccountDeleteParams {
  provider: string
  accountIds: string[]
}

interface AccountImportSettings {
  notes?: string
  enabled: boolean
  concurrencyLimit: number | null
  weight: number
  modelAccess?: AccountModelAccess
  groupIds: string[]
}

interface AccountImportParam {
  outboundProxyId?: string
  settings?: AccountImportSettings
  provider: string
  data: unknown
}

interface AccountImportTaskIdParam {
  taskId: string
}

interface CreateAccountImportTaskParam {
  submissionId: string
  items: AccountImportParam[]
}

interface AccountOAuthStartParam {
  outboundProxyUrl?: string
  outboundProxyId?: string
  provider: string
  name: string
  accountId?: string
}

interface AccountOAuthCompleteParam {
  settings?: AccountImportSettings
  provider: string
  flowId: string
  callbackUrl: string
}

interface AccountExportParam {
  accountIds: string
  confirm: string
}

export function getAccounts(data: AccountListParams, options: RequestOptions = {}) {
  return request<AccountListResponse>({
    url: '/api/admin/accounts',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function exportAccounts(data: AccountExportParam) {
  return request<unknown>({
    url: '/api/admin/accounts/export',
    method: 'GET',
    params: data,
  })
}

export function refreshAccount(data: AccountIdParam) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/refresh',
    method: 'POST',
    data,
  })
}

export function recoverAccount(data: AccountIdParam) {
  return request<AccountRefreshResponse>({
    url: '/api/admin/accounts/recover',
    method: 'POST',
    data,
  })
}

export function getAccountPersonalInfo(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountPersonalInfoResponse>({
    url: '/api/admin/accounts/personal-info',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function getAccountQuotaForecast(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountQuotaForecastResponse>({
    url: '/api/admin/accounts/quota-forecast',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function accountProfileAvatarUrl(accountId: string, sourceUrl: string) {
  const params = new URLSearchParams({
    accountId,
    version: stableAvatarVersion(sourceUrl),
  })
  return `/api/admin/accounts/profile-avatar?${params.toString()}`
}

function stableAvatarVersion(value: string) {
  let hash = 0
  for (const character of value)
    hash = (hash * 33 + (character.codePointAt(0) ?? 0)) % 2_147_483_647
  return hash.toString(36)
}

export function refreshAccountQuota(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountQuotaResponse>({
    url: '/api/admin/accounts/quota/refresh',
    method: 'POST',
    data,
    ...options,
  })
}

export function getAccountResetCredits(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountResetCreditsResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function consumeAccountResetCredit(data: AccountResetCreditConsumeParam, options: RequestOptions = {}) {
  return request<AccountResetCreditResultResponse>({
    url: '/api/admin/accounts/reset-credits',
    method: 'POST',
    data,
    ...options,
  })
}

export function getAccountModels(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function refreshAccountModels(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountModelsResponse>({
    url: '/api/admin/accounts/models/refresh',
    method: 'POST',
    data,
    ...options,
  })
}

export function importAccounts(data: AccountImportParam, options: RequestOptions = {}) {
  return request<AccountImportResponse>({
    url: '/api/admin/accounts/import',
    method: 'POST',
    data,
    ...options,
  })
}

export function createAccountImportTask(data: CreateAccountImportTaskParam) {
  return request<AccountImportTask>({
    url: '/api/admin/accounts/import-tasks',
    method: 'POST',
    data,
  })
}

export function getAccountImportTasks(options: RequestOptions = {}) {
  return request<{ items: AccountImportTask[] }>({
    url: '/api/admin/accounts/import-tasks',
    method: 'GET',
    ...options,
  })
}

export function getAccountImportTask(data: AccountImportTaskIdParam, options: RequestOptions = {}) {
  return request<AccountImportTaskDetail>({
    url: '/api/admin/accounts/import-tasks/detail',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function stopAccountImportTask(data: AccountImportTaskIdParam) {
  return request<AccountImportTaskDetail>({
    url: '/api/admin/accounts/import-tasks/stop',
    method: 'POST',
    data,
  })
}

export function updateAccount(data: AccountUpdateParam) {
  return request<AccountUpdateResponse>({
    url: '/api/admin/accounts/update',
    method: 'POST',
    data,
  })
}

export function batchUpdateAccounts(data: AccountBatchUpdateParam) {
  return request<AccountBatchUpdateResponse>({
    url: '/api/admin/accounts/batch-update',
    method: 'POST',
    data,
  })
}

export function deleteAccounts(data: AccountDeleteParams, options: RequestOptions = {}) {
  return request<AccountDeletionResponse>({
    url: '/api/admin/accounts/delete',
    method: 'POST',
    data,
    ...options,
  })
}

export function startAccountOAuth(data: AccountOAuthStartParam) {
  return request<AccountOAuthStartResponse>({
    url: '/api/admin/accounts/oauth/start',
    method: 'POST',
    data,
  })
}

export function completeAccountOAuth(data: AccountOAuthCompleteParam) {
  return request<AccountOAuthCompleteResponse>({
    url: '/api/admin/accounts/oauth/complete',
    method: 'POST',
    data,
  })
}

export interface ApiKeyConfiguration {
  base_url: string
  transport: 'http' | 'prefer_websocket'
}

export interface TurnStatePinStatus {
  model: string
  length: number
  capturedAt: string
  expiresAt: string
  hits: number
  /** account：遍历代理后钉住，对全部客户端生效；client：按客户端密钥被动捕获。 */
  scope?: 'account' | 'client'
}

export interface TurnStateHuntProxy {
  /** null 表示直连 */
  proxyId: string | null
  name: string
  endpoint: string | null
}

export interface TurnStateHuntAttemptError {
  code: string
  source: 'gateway' | 'provider' | 'upstream'
  upstreamStatus: number | null
  message: string
}

/** 遍历代理找 state 的 SSE 事件；只有 state 的字节数，没有值。 */
export type TurnStateHuntEvent
  = | { type: 'hunt_start', model: string, expectedLength: number, attempts: number, proxies: TurnStateHuntProxy[] }
    | ({ type: 'proxy_start', index: number, total: number } & TurnStateHuntProxy)
    | { type: 'attempt', proxyId: string | null, index: number, length: number | null, matched: boolean, error: TurnStateHuntAttemptError | null }
    | { type: 'proxy_done', proxyId: string | null, attempts: number, matched: boolean, skipped: 'unavailable' | 'unreachable' | 'capacity' | null }
    | { type: 'hit', proxyId: string | null, attemptIndex: number, length: number }
    | { type: 'bound', proxyId: string | null, changed: boolean }
    | { type: 'pinned', model: string, length: number, expiresAt: string }
    | { type: 'hunt_complete', success: boolean, requests: number }
    | { type: 'error', code: string, message: string }

export interface TurnStateHuntParam {
  accountId: string
  modelId: string
  attempts: number
  includeDirect: boolean
  /** 只遍历这一个代理；缺省遍历全部已测试通过的代理。 */
  proxyId?: string | null
}

export function turnStateHuntStreamUrl(params: TurnStateHuntParam) {
  const query = new URLSearchParams({
    accountId: params.accountId,
    modelId: params.modelId,
    attempts: String(params.attempts),
    includeDirect: String(params.includeDirect),
  })
  if (params.proxyId)
    query.set('proxyId', params.proxyId)
  return `${API_BASE_URL}/api/admin/accounts/turn-state-hunt?${query}`
}

/** 自动撞 state：从轮换代理模板即时生成多国临时出口反复撞，命中即切静态。事件形状同遍历。 */
export interface TurnStateAutoHuntParam {
  accountId: string
  modelId: string
  /** 轮换代理模板的代理 id（如 dongtai-US）。 */
  templateProxyId: string
  /** 出口国家代码，US/JP/DE/PH。 */
  countries: string[]
  /** 命中后可改绑的静态出口 id；后端挑其中账号数最少的一个。 */
  staticProxyIds: string[]
  /** 最多生成多少个临时 IP（额度闸）。 */
  maxIps: number
}

export function turnStateAutoHuntStreamUrl(params: TurnStateAutoHuntParam) {
  const query = new URLSearchParams({
    accountId: params.accountId,
    modelId: params.modelId,
    templateProxyId: params.templateProxyId,
    countries: params.countries.join(','),
    staticProxyIds: params.staticProxyIds.join(','),
    maxIps: String(params.maxIps),
  })
  return `${API_BASE_URL}/api/admin/accounts/turn-state-auto-hunt?${query}`
}

export interface TurnStateCaptureRule {
  defaultLength: number | null
  modelLengths: Record<string, number>
}

/** 账号级 state 到期前自动重新遍历代理的参数；null 表示未开启。 */
export interface TurnStateAutoHunt {
  modelId: string
  attempts: number
  includeDirect: boolean
}

export interface OAuthStateConfiguration {
  guanlanReviveAvailable?: boolean
  pinTurnState: boolean
  turnStateAutoHunt?: TurnStateAutoHunt | null
  turnStatePins: TurnStatePinStatus[]
  turnStateCaptureRule?: TurnStateCaptureRule
  maxAgeSeconds: number
}

export function updateAccountTurnState(data: {
  accountId: string
  pinTurnState?: boolean
  turnStateAutoHunt?: { enabled: false } | ({ enabled: true } & TurnStateAutoHunt)
  settings?: AccountUpdateParam
}) {
  return request<{ accountId: string }>({
    url: '/api/admin/accounts/rotate',
    method: 'POST',
    data: { provider: 'openai', ...data },
  })
}

export function getAccountTicket(data: AccountIdParam, options: RequestOptions = {}) {
  return request<AccountTicket>({
    url: '/api/admin/accounts/ticket',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function updateAccountTicket(data: AccountTicketUpdate) {
  return request<AccountTicket>({
    url: '/api/admin/accounts/ticket',
    method: 'POST',
    data,
  })
}

/** 用票据经服务端登录换回令牌并写回原账号；登录含 2FA 与人机校验，耗时较长。 */
export function restoreAccountFromTicket(data: AccountIdParam) {
  return request<{ accountId: string }>({
    url: '/api/admin/accounts/ticket/restore',
    method: 'POST',
    data,
    timeout: 4 * 60 * 1000,
  })
}

export function reviveGuanlanAccount(data: AccountIdParam) {
  return request<{ accountId: string }>({
    url: '/api/admin/accounts/rotate',
    method: 'POST',
    data: { provider: 'openai', ...data, guanlanRevive: true },
    timeout: 65 * 60 * 1000,
  })
}

export function getAccountDetail(data: AccountIdParam, options: RequestOptions = {}) {
  return request<{ account: Account, credentialConfiguration?: ApiKeyConfiguration | OAuthStateConfiguration }>({
    url: '/api/admin/accounts/detail',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function updateAccountApiKey(data: { accountId: string, baseUrl: string, transport: ApiKeyConfiguration['transport'], apiKey?: string, settings?: AccountUpdateParam }) {
  return request<{ accountId: string }>({
    url: '/api/admin/accounts/rotate',
    method: 'POST',
    data: { provider: 'openai', ...data },
  })
}
