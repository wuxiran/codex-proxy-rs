<script setup lang="ts">
import type { GuanlanReviveStatus } from '@/api'
import { computed } from 'vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'

const props = defineProps<{ status: GuanlanReviveStatus | null, loading: boolean, ready: boolean, saving: boolean }>()
const enabled = defineModel<boolean>({ required: true })
const statusLabels: Record<string, string> = {
  disabled: '未开启自动复活',
  service_disabled: '服务器已停用自动复活服务',
  paused: '账号调度已暂停，暂不自动复活',
  waiting: '已开启，等待凭据失效',
  running: '正在向观澜申请复活',
  recovered: '最近一次复活成功',
  failed: '最近一次复活未成功',
  unavailable: '暂不具备自动复活条件',
}
const reasons: Record<string, string> = {
  missing_signed_export: '未找到这个账号的观澜签名原件，请通过观澜 CDK 或原始签名 JSON 重新导入。',
  invalid_signed_export: '签名原件不完整或账号身份不匹配，请重新导入观澜原始文件。',
  batch_requires_all_accounts: '同一签名文件中的账号需全部开启自动复活且凭据已失效，避免影响其他账号。',
  rate_limited: '观澜暂时限流，稍后自动重试。',
  snapshot_expired: '观澜检测结果已过期，稍后重新检测。',
  rejected: '观澜拒绝了签名文件或复活申请。',
  timeout: '观澜处理超时，稍后自动重试。',
  identity_mismatch: '恢复结果的账号身份不匹配，未写回凭据。',
  not_recovered: '观澜未返回可恢复的账号凭据。',
  partial_recovery: '部分账号未恢复，将稍后重试。',
  account_changed: '恢复期间账号已被修改，本次未覆盖新设置。',
  failed: '复活服务暂时不可用，稍后自动重试。',
}
const canEnable = computed(() => props.ready && props.status?.eligible && props.status.serviceEnabled)
const label = computed(() => {
  if (props.loading)
    return '正在读取复活设置…'
  if (!props.ready)
    return '复活设置读取失败，请重新打开账号。'
  if (!props.status)
    return '当前服务暂不支持自动复活设置。'
  if (enabled.value !== props.status.enabled)
    return enabled.value ? '保存后开启自动复活' : '保存后关闭自动复活'
  return statusLabels[props.status.status] ?? '状态待确认'
})
function date(seconds: number) {
  return new Date(seconds * 1000).toLocaleString()
}
</script>

<template>
  <section class="grid gap-3 rounded-cp bg-cp-fill-quaternary p-4" aria-label="观澜自动复活">
    <div class="flex items-center justify-between gap-4">
      <div class="min-w-0">
        <h3 class="m-0 text-cp font-heavy text-cp-text">
          观澜自动复活
        </h3>
        <p class="mb-0 mt-1 text-cp-sm text-cp-text-secondary">
          {{ status?.source === 'guanlan' ? '账号来源：观澜 · 签名原件已归档' : '账号来源：尚未确认观澜签名原件' }}
        </p>
      </div>
      <BaseSwitch v-model="enabled" label="401 后自动复活" :disabled="saving || !ready || (!enabled && !canEnable)" />
    </div>
    <p class="m-0 text-cp-sm text-cp-text-secondary">
      开启后，凭据失效时自动向观澜提交该账号的签名原件，恢复成功后更新凭据。额度耗尽、限流或普通网络错误不会触发。
    </p>
    <p role="status" class="m-0 text-cp-sm" :class="status?.status === 'failed' ? 'text-cp-error' : 'text-cp-text-secondary'">
      {{ label }}
    </p>
    <p v-if="status?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
      {{ reasons[status.reason] ?? '请检查观澜签名文件及服务状态。' }}
    </p>
    <dl v-if="status?.lastAttemptAt || status?.nextAttemptAt" class="m-0 grid gap-1 text-cp-sm text-cp-text-secondary">
      <div v-if="status.lastAttemptAt" class="flex flex-wrap gap-x-2">
        <dt>上次尝试</dt><dd class="m-0">
          {{ date(status.lastAttemptAt) }}
        </dd>
      </div>
      <div v-if="status.nextAttemptAt && status.status === 'failed'" class="flex flex-wrap gap-x-2">
        <dt>下次重试</dt><dd class="m-0">
          {{ date(status.nextAttemptAt) }}
        </dd>
      </div>
    </dl>
  </section>
</template>
