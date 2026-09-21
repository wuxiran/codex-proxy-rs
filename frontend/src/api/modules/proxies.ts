import type { RequestOptions } from '../request'
import type { RequestLocation } from '../types/request-location'
import type { AccountGroupRef } from './account-groups'
import request from '../request'

export interface OutboundProxyExitGeo {
  country: string
  countryCode: string
  region: string | null
  city: string | null
}

export interface OutboundProxyTest {
  success: boolean
  latencyMs: number
  exitIp: string | null
  // 滚动发布期间旧后端不返回地区，按缺失处理。
  exitGeo?: OutboundProxyExitGeo | null
  message: string
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

export interface OutboundProxyRecord {
  location: RequestLocation | null
  id: string
  name: string
  endpoint: string
  hasAuthentication: boolean
  revision: number
  accountCount: number
  lastTestAt: string | null
  lastTest: OutboundProxyTest | null
  quality?: ProxyQualitySnapshot | null
  createdAt: string
  updatedAt: string
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

export function createProxy(data: { name: string, proxyUrl: string, location?: RequestLocation | null }) {
  return request<ProxyMutation>({
    url: '/api/admin/proxies/create',
    method: 'POST',
    data,
  })
}

export function updateProxy(data: { id: string, revision: number, name: string, proxyUrl?: string, location?: RequestLocation | null }) {
  return request<ProxyMutation>({
    url: '/api/admin/proxies/update',
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

// 槽位排队最多 20 秒，其后才是探测本身；超时需要覆盖两段。
export function testProxy(data: { id: string, revision: number }, options: RequestOptions = {}) {
  return request<OutboundProxyRecord>({
    url: '/api/admin/proxies/test',
    method: 'POST',
    data,
    timeout: 45000,
    ...options,
  })
}

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

export function batchCreateProxies(data: { items: Array<{ name?: string, proxyUrl: string }> }) {
  return request<{ created: OutboundProxyRecord[], skipped: ProxyBatchSkip[], configRevision: number | null }>({
    url: '/api/admin/proxies/batch-create',
    method: 'POST',
    data,
    timeout: 60000,
  })
}

export function batchDeleteProxies(data: { items: Array<{ id: string, revision: number }> }) {
  return request<{ deletedIds: string[], skipped: ProxyBatchSkip[], configRevision: number | null }>({
    url: '/api/admin/proxies/batch-delete',
    method: 'POST',
    data,
    timeout: 60000,
  })
}

export function probeProxy(data: { proxyUrl: string }) {
  return request<OutboundProxyTest>({
    url: '/api/admin/proxies/probe',
    method: 'POST',
    data,
    timeout: 25000,
  })
}
