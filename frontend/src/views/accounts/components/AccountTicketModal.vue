<script setup lang="ts">
import type { Account } from '@/api'
import type { AccountTicket } from '@/api/modules/accounts'
import { ZButton, ZDialog, ZFormItem, ZInput, ZNotification, ZSegmented, ZTextarea } from '@codex-proxy/ui'

import { computed, shallowRef, watch } from 'vue'
import { restoreAccountFromTicket, updateAccountTicket } from '@/api'
import { formatDateTimeLocalInput } from '@/utils/format.fork'
import { errorMessage } from '@/utils/operation'

const props = defineProps<{ account: Account | null }>()
const emit = defineEmits<{ changed: [] }>()
const open = defineModel<boolean>({ default: false })

const amount = shallowRef('')
const currency = shallowRef<'CNY' | 'USD'>('CNY')
const purchasedAt = shallowRef('')
const expiresAt = shallowRef('')
const ticketLine = shallowRef('')
const current = shallowRef<AccountTicket | null>(null)
const saving = shallowRef(false)
const restoring = shallowRef(false)

const currencyOptions = [
  { label: '人民币 ¥', value: 'CNY' },
  { label: '美元 $', value: 'USD' },
]
const busy = computed(() => saving.value || restoring.value)
const oauth = computed(() => props.account?.provider === 'openai' && props.account.authenticationKind === 'oauth')

function toLocalInput(value: string | null) {
  return formatDateTimeLocalInput(value)
}

function toIso(value: string) {
  if (!value)
    return null
  const time = new Date(value).getTime()
  return Number.isNaN(time) ? null : new Date(time).toISOString()
}

watch([open, () => props.account], ([isOpen, account]) => {
  if (!isOpen || !account)
    return
  const ticket = account.ticket
  current.value = ticket
  amount.value = ticket.purchaseAmount ?? ''
  currency.value = ticket.purchaseCurrency ?? 'CNY'
  purchasedAt.value = toLocalInput(ticket.purchasedAt)
  expiresAt.value = toLocalInput(ticket.expiresAt)
  ticketLine.value = ''
}, { immediate: true })

async function save(options: { clearTicket?: boolean } = {}) {
  if (!props.account)
    return
  saving.value = true
  try {
    current.value = await updateAccountTicket({
      accountId: props.account.id,
      purchaseAmount: amount.value.trim() || null,
      purchaseCurrency: amount.value.trim() ? currency.value : null,
      purchasedAt: toIso(purchasedAt.value),
      expiresAt: toIso(expiresAt.value),
      ticket: ticketLine.value.trim() || undefined,
      clearTicket: options.clearTicket,
    })
    ticketLine.value = ''
    ZNotification.success({ message: options.clearTicket ? '已删除票据' : '已保存' })
    emit('changed')
  }
  catch (error: unknown) {
    ZNotification.error({ message: errorMessage(error, '保存失败') })
  }
  finally {
    saving.value = false
  }
}

async function restore() {
  if (!props.account)
    return
  restoring.value = true
  try {
    await restoreAccountFromTicket({ accountId: props.account.id })
    ZNotification.success({ message: '已用票据重新登录并写回令牌' })
    emit('changed')
  }
  catch (error: unknown) {
    ZNotification.error({ message: errorMessage(error, '票据恢复失败'), duration: 8000 })
  }
  finally {
    restoring.value = false
  }
}
</script>

<template>
  <ZDialog v-model="open" title="成本与票据" width="36rem" :show-close="!busy" :close-on-click-modal="!busy" :close-on-press-escape="!busy">
    <div v-if="account" class="grid gap-4">
      <p class="m-0 text-cp-sm text-cp-text-secondary">
        {{ account.email ?? account.name }}
      </p>

      <ZFormItem label="买入价" description="留空表示不记录成本；「已刷」按买入时间起的模型价格计费累计">
        <div class="flex flex-wrap items-center gap-2">
          <ZInput v-model="amount" class="max-w-40" inputmode="decimal" placeholder="例如 55" aria-label="买入价" :disabled="busy" />
          <ZSegmented v-model="currency" aria-label="币种" :options="currencyOptions" :disabled="busy" />
        </div>
      </ZFormItem>

      <div class="grid gap-4 sm:grid-cols-2">
        <ZFormItem label="买入时间">
          <ZInput v-model="purchasedAt" type="datetime-local" aria-label="买入时间" :disabled="busy" />
        </ZFormItem>
        <ZFormItem label="到期时间" description="例如车主说 23:20 踢人；列表显示倒计时">
          <ZInput v-model="expiresAt" type="datetime-local" aria-label="到期时间" :disabled="busy" />
        </ZFormItem>
      </div>

      <ZFormItem
        v-if="oauth"
        label="登录票据"
        :description="current?.hasTicket
          ? `已保存（${current.ticketHint}）。粘贴新票据可替换；票据加密存储，保存后不再显示明文`
          : '一行：邮箱----密码----2FA密钥。加密存储，保存后不再显示明文'"
      >
        <ZTextarea
          v-model="ticketLine"
          :rows="2"
          autocomplete="off"
          spellcheck="false"
          placeholder="name@example.com----password----BASE32SECRET"
          :disabled="busy"
        />
      </ZFormItem>

      <section v-if="oauth && current?.hasTicket" class="grid gap-2 rounded-cp bg-cp-fill-quaternary p-3">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <span class="text-cp-sm text-cp-text-secondary">账号掉线或令牌失效时，用票据经服务端重新登录（走该账号绑定的出口）并写回。</span>
          <div class="flex items-center gap-2">
            <ZButton size="small" :disabled="busy" @click="save({ clearTicket: true })">
              删除票据
            </ZButton>
            <ZButton size="small" type="primary" :loading="restoring" :disabled="busy" @click="restore">
              用票据恢复
            </ZButton>
          </div>
        </div>
        <p v-if="restoring" role="status" class="m-0 text-cp-xs text-cp-text-tertiary">
          正在登录（含 2FA 与人机校验），通常需要 30 秒到 2 分钟…
        </p>
      </section>
    </div>

    <template #footer>
      <ZButton :disabled="busy" @click="open = false">
        关闭
      </ZButton>
      <ZButton type="primary" :loading="saving" :disabled="busy || !account" @click="save()">
        保存
      </ZButton>
    </template>
  </ZDialog>
</template>
