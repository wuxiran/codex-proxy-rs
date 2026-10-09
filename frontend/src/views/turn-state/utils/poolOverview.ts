import type { PoolAccount, PoolConnection } from '@/api'

export const poolPhaseText: Record<PoolAccount['phase'], string> = {
  inactive: '未参与',
  unavailable: '账号不可调度',
  verifying: '正在验证',
  minting: '正在打票',
  ready: '有可用连接',
  cooling: '冷却中',
  idle: '等待准备',
}

export const verificationText: Record<PoolConnection['verification'], string> = {
  fresh: '验证有效',
  unchecked: '未检查答案',
  expired: '验证已过期',
  conditions_changed: '验证条件已变化',
  pending: '等待验证',
  rejected: '验证未通过',
  closed: '连接已关闭',
}

export function remaining(until: number | null | undefined, now: number) {
  if (!until)
    return '未记录'
  const seconds = Math.max(0, Math.ceil((until - now) / 1000))
  if (!seconds)
    return '已到期'
  if (seconds < 60)
    return `${seconds} 秒`
  return `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`
}

export function latestAttempt(account: PoolAccount) {
  return [account.mint.last, account.warm.last].filter(value => value !== null).sort((a, b) => b.atMs - a.atMs)[0]
}
