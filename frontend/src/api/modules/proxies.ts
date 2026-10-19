import type { RequestOptions } from '../request'
import type { AccountGroupRef } from './groups'
import type { RequestLocation } from './settings'
import request from '../request'

// fork: proxy-quality — 出口地理与质量检测的类型
export interface OutboundProxyExitGeo {
  country: string
  countryCode: string
  region: string | null
  city: string | null
}

export type ProxyQualityStatus = 'healthy' | 'warn' | 'challenge' | 'failed'
export type ProxyQualityItemStatus = 'pass' | 'warn' | 'fail' | 'challenge'

export interface ProxyQualitySnapshot {
  score: number
  grade: string
  status: ProxyQualityStatus
  summary: string
  checkedAt: string
}

export interface ProxyQualityItem {
  target: string
  status: ProxyQualityItemStatus
  httpStatus: number | null
  latencyMs: number | null
  message: string
  cfRay: string | null
}

export interface ProxyQualityReport extends ProxyQualitySnapshot {
  exitIp: string | null
  exitGeo: OutboundProxyExitGeo | null
  baseLatencyMs: number | null
  passedCount: number
  warnCount: number
  failedCount: number
  challengeCount: number
  items: ProxyQualityItem[]
}

export interface ProxyBatchSkip {
  reference: string
  reason: string
}

export type ProxyLocationDetection
  = | { status: 'notRequested' }
    | { status: 'detected', location: RequestLocation }
    | { status: 'failed', message: string }
    | { status: 'conflict' }

export interface DetectedProxyLocation {
  location: RequestLocation
  exitIpv4: string | null
  exitIpv6: string | null
  detectedAt: string
}

export interface OutboundProxyTest {
  location: ProxyLocationDetection
  success: boolean
  latencyMs: number
  exitIp: string | null
  exitIpv4: string | null
  exitIpv6: string | null
  exitGeo?: OutboundProxyExitGeo | null // fork: proxy-quality
  message: string
}

export interface OutboundProxyRecord {
  maxWebsocketConnections: number
  enabled: boolean
  autoLocation: boolean
  detectedLocation: DetectedProxyLocation | null
  location: RequestLocation | null
  id: string
  name: string
  endpoint: string
  hasAuthentication: boolean
  revision: number
  accountCount: number
  lastTestAt: string | null
  lastTestAtDisplay: string | null
  lastTest: OutboundProxyTest | null
  quality?: ProxyQualitySnapshot | null // fork: proxy-quality
  createdAt: string
  createdAtDisplay: string
  updatedAt: string
  updatedAtDisplay: string
}

interface ProxyPage {
  items: OutboundProxyRecord[]
  page: { page: number, pageSize: number, total: number, totalPages: number }
}

export interface OutboundProxyAccount {
  id: string
  name: string
  email: string | null
  provider: string
  authenticationKind: string
  planType: string | null
  planTypeDisplay: string | null
  groups: AccountGroupRef[]
  enabled: boolean
}

interface ProxyAccountPage {
  items: OutboundProxyAccount[]
  page: ProxyPage['page']
}

export function getProxyAccounts(data: { proxyId: string, page: number, pageSize: number, search?: string }, options: RequestOptions = {}) {
  return request<ProxyAccountPage>({
    url: '/api/admin/proxies/accounts',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function removeProxyAccount(data: { proxyId: string, accountId: string }) {
  return request<{ configRevision: number }>({
    url: '/api/admin/proxies/accounts/remove',
    method: 'POST',
    data,
  })
}

export type ProxyBatchResult<T>
  = | { status: 'succeeded', data: T }
    | { status: 'failed', httpStatus: number, code: number, message: string }
    | { status: 'skipped' }

interface ProxyMutation {
  record: OutboundProxyRecord
  configRevision: number
}

export function getProxies(data: { page: number, pageSize: number, search?: string }, options: RequestOptions = {}) {
  return request<ProxyPage>({
    url: '/api/admin/proxies',
    method: 'GET',
    params: data,
    ...options,
  })
}

export function createProxy(data: { name: string, proxyUrl: string, maxWebsocketConnections?: number, autoLocation?: boolean, location?: RequestLocation | null }[], options: RequestOptions = {}) {
  return request<ProxyBatchResult<ProxyMutation>[]>({
    url: '/api/admin/proxies/create',
    timeout: 25000 * Math.max(1, data.length),
    method: 'POST',
    data,
    ...options,
  })
}

export function updateProxy(data: { id: string, revision: number, name: string, enabled?: boolean, maxWebsocketConnections?: number, proxyUrl?: string, autoLocation?: boolean, location?: RequestLocation | null }) {
  return request<ProxyMutation>({
    url: '/api/admin/proxies/update',
    timeout: 25000,
    method: 'POST',
    data,
  })
}

export function deleteProxy(data: { id: string, revision: number }) {
  return request<{ configRevision: number }>({
    url: '/api/admin/proxies/delete',
    method: 'POST',
    data,
  })
}

export function testProxy(data: { id: string, revision: number, detectLocation?: boolean }[], options: RequestOptions = {}) {
  return request<ProxyBatchResult<OutboundProxyRecord>[]>({
    url: '/api/admin/proxies/test',
    method: 'POST',
    data,
    timeout: 25000 * Math.max(1, Math.ceil(data.length / 2)),
    ...options,
  })
}

// fork: proxy-quality — 质量检测与报告读取
export function checkProxyQuality(data: { id: string, revision: number }, options: RequestOptions = {}) {
  return request<{ record: OutboundProxyRecord, report: ProxyQualityReport }>({
    url: '/api/admin/proxies/quality-check',
    method: 'POST',
    data,
    timeout: 75000,
    ...options,
  })
}

export function getProxyQualityReport(data: { id: string }, options: RequestOptions = {}) {
  return request<{ report: ProxyQualityReport | null }>({
    url: '/api/admin/proxies/quality-report',
    method: 'GET',
    params: data,
    ...options,
  })
}

// fork: proxy-batch-delete — 上游只有单条删除；批量删除沿用 fork 后端接口
export function batchDeleteProxies(data: { items: Array<{ id: string, revision: number }> }) {
  return request<{ deletedIds: string[], skipped: ProxyBatchSkip[], configRevision: number | null }>({
    url: '/api/admin/proxies/batch-delete',
    method: 'POST',
    data,
    timeout: 60000,
  })
}

export function probeProxy(data: { proxyUrl: string, detectLocation?: boolean }) {
  return request<OutboundProxyTest>({
    url: '/api/admin/proxies/probe',
    method: 'POST',
    data,
    timeout: 25000,
  })
}
