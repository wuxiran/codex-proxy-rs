<script setup lang="ts">
import type { Account, OAuthStateConfiguration, TurnStateAutoHunt } from '@/api'
import { BaseButton, BaseModal } from '@codex-proxy/ui'
import { ref } from 'vue'
import { formatDateTime } from '@/utils/format'
import AccountTurnStateHistory from './AccountTurnStateHistory.vue'
import AccountTurnStateHunt from './AccountTurnStateHunt.vue'

defineProps<{ account: Account, configuration?: OAuthStateConfiguration, busy: boolean }>()
const emit = defineEmits<{
  recapture: []
  stopAutoHunt: []
  huntBusy: [busy: boolean]
  hunted: [boundChanged: boolean, autoRenew: TurnStateAutoHunt | null]
  huntCancelled: []
}>()
const open = defineModel<boolean>({ required: true })
const showHistory = ref(false)
const showHunt = ref(false)
const hunting = ref(false)
const sourceText = { mint: '专用打票', hunt: '遍历代理', passive: '请求捕获', renewal: '自动续期' }
function onHuntBusy(value: boolean) {
  hunting.value = value
  emit('huntBusy', value)
}
</script>

<template>
  <BaseModal v-model="open" title="账号票据详情" size="md-wide" :dismissible="!busy && !hunting">
    <div class="grid min-w-0 gap-4">
      <div class="truncate font-emphasis" :title="account.name">
        {{ account.name }}
      </div>
      <p v-if="!configuration" class="m-0 text-cp-sm text-cp-error" role="alert">
        状态读取失败，请关闭后刷新账号
      </p>
      <template v-else>
        <div class="grid gap-2 text-cp-sm sm:grid-cols-2">
          <span>票与预热：{{ configuration.pinTurnState ? '已启用' : '未启用' }}</span>
          <span>可用预热连接：{{ configuration.warmPool?.held ?? '未记录' }}</span>
        </div>
        <div v-if="configuration.turnStatePins.length" class="grid gap-2">
          <div v-for="(pin, index) in configuration.turnStatePins" :key="`${pin.model}-${index}`" class="rounded-cp bg-cp-fill-quaternary p-3 text-cp-sm">
            <div>{{ pin.model }} · {{ pin.source ? sourceText[pin.source] : '来源未记录' }} · {{ pin.scope === 'account' ? '账号共享' : '客户端专用' }}</div>
            <div class="mt-1 text-cp-xs text-cp-text-secondary">
              {{ formatDateTime(pin.expiresAt) }} 到期 · 使用 {{ pin.hits }} 次
            </div>
          </div>
        </div>
        <p v-else class="m-0 text-cp-sm text-cp-text-secondary">
          暂无有效票据，预热连接状态单独显示
        </p>
        <details>
          <summary class="cursor-pointer text-cp-sm text-cp-link">
            高级操作
          </summary>
          <div class="mt-3 grid min-w-0 gap-3">
            <div class="flex flex-wrap gap-2">
              <BaseButton size="sm" variant="soft" :disabled="busy || hunting || !configuration.pinTurnState" @click="showHunt = !showHunt">
                遍历代理
              </BaseButton>
              <BaseButton size="sm" variant="soft" :disabled="busy" @click="showHistory = !showHistory">
                最近记录
              </BaseButton>
              <BaseButton size="sm" variant="secondary" :disabled="busy || hunting || !configuration.pinTurnState" @click="emit('recapture')">
                重新捕获
              </BaseButton>
            </div>
            <div v-if="configuration.turnStateAutoHunt" class="flex flex-wrap items-center gap-2 text-cp-sm">
              自动遍历续期：{{ configuration.turnStateAutoHunt.modelId }}
              <BaseButton size="sm" variant="soft" :disabled="busy || hunting" @click="emit('stopAutoHunt')">
                停止续期
              </BaseButton>
            </div>
            <AccountTurnStateHunt
              v-if="showHunt" :account-id="account.id" :capture-rule="configuration.turnStateCaptureRule ?? null"
              @busy="onHuntBusy" @hunted="(changed, renewal) => emit('hunted', changed, renewal)"
              @cancelled="emit('huntCancelled')" @close="showHunt = false"
            />
            <AccountTurnStateHistory v-if="showHistory" :account-id="account.id" @close="showHistory = false" />
          </div>
        </details>
      </template>
    </div>
  </BaseModal>
</template>
