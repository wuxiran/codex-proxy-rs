<script setup lang="ts">
import type { AccountRow } from '../constants'
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'
import type { AccountGroup, AccountModelAccess, TurnStateCaptureRule, TurnStatePinStatus } from '@/api'

import { ref, useId, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import AccountApiKeyFields from './AccountApiKeyFields.vue'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountPlanBadge from './AccountPlanBadge.vue'
import AccountSettingsFields from './AccountSettingsFields.vue'
import AccountTurnStateHistory from './AccountTurnStateHistory.vue'

defineProps<{
  account: AccountRow | null
  groups: AccountGroup[]
  groupsLoading: boolean
  saving: boolean
  configurationLoading: boolean
  configurationReady: boolean
  turnStatePins: TurnStatePinStatus[]
  turnStateCaptureRule: TurnStateCaptureRule | null
}>()

const emit = defineEmits<{
  save: []
}>()

const open = defineModel<boolean>({ required: true })
const showStateHistory = ref(false)
const historyId = useId()
watch(open, (value) => {
  if (!value)
    showStateHistory.value = false
})
const apiKey = defineModel<ApiKeyAccountForm>('apiKey', { required: true })
const pinTurnState = defineModel<boolean>('pinTurnState', { required: true })
const recaptureTurnState = defineModel<boolean>('recaptureTurnState', { required: true })
const notes = defineModel<string>('notes', { required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <BaseModal
    v-model="open"
    title="编辑账号"
    size="lg"
    :dismissible="!saving"
  >
    <div v-if="account" class="grid gap-5">
      <div
        class="flex flex-wrap items-center justify-between gap-4 rounded-cp bg-cp-fill-quaternary px-4 py-3.5"
      >
        <AccountIdentityCell
          class="min-w-0 flex-1"
          :account="account"
          size="lg"
        />
        <div class="flex shrink-0 items-center gap-3">
          <AccountPlanBadge :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" />
          <ProviderIconGroup
            :provider="account.provider"
            :authentication-kind="account.authenticationKind"
          />
        </div>
      </div>

      <section v-if="account.authenticationKind === 'api_key'" class="grid gap-4">
        <h3 class="m-0 text-cp font-heavy text-cp-text">
          上游连接
        </h3>
        <p v-if="configurationLoading" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
          正在读取上游设置…
        </p>
        <p v-else-if="!configurationReady" role="alert" class="m-0 text-cp-sm text-cp-error">
          上游设置读取失败，请关闭后重试
        </p>
        <AccountApiKeyFields v-else v-model="apiKey" editing :disabled="saving" />
      </section>

      <section v-if="account.provider === 'openai' && account.authenticationKind === 'oauth'" class="grid gap-3 rounded-cp bg-cp-fill-quaternary p-4" aria-label="固定自身 state">
        <div class="flex flex-wrap items-center justify-between gap-3">
          <h3 class="m-0 text-cp font-heavy text-cp-text">
            固定自身 state <span class="text-cp-sm text-cp-text-secondary">（实验）</span>
          </h3>
          <div class="flex items-center gap-3">
            <BaseButton variant="soft" size="sm" :disabled="saving" :aria-expanded="showStateHistory" :aria-controls="historyId" @click="showStateHistory = !showStateHistory">
              最近捕获
            </BaseButton>
            <BaseSwitch v-model="pinTurnState" label="固定自身 state" :disabled="saving || !configurationReady" />
          </div>
        </div>
        <p v-if="configurationLoading" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
          正在读取状态设置…
        </p>
        <p v-else-if="!configurationReady" role="alert" class="m-0 text-cp-sm text-cp-error">
          state 设置读取失败；其他账号设置仍可保存，请重新打开后再修改此开关
        </p>
        <template v-else>
          <p class="m-0 text-cp-sm text-cp-text-secondary">
            开启后，捕获本账号完整成功请求返回的首个符合下方规则的 state，按模型和客户端密钥分别固定。后续返回的新 state 不覆盖它。
          </p>
          <div v-if="turnStateCaptureRule" class="grid gap-1 text-cp-xs text-cp-text-secondary" aria-label="state 捕获规则">
            <span v-for="(length, model) in turnStateCaptureRule.modelLengths" :key="model">
              {{ model }}：{{ length }} 字节
            </span>
            <span v-if="turnStateCaptureRule.defaultLength !== null">默认规则：{{ turnStateCaptureRule.defaultLength }} 字节</span>
            <span v-else>仅捕获上述模型，未列出的模型等待补充规则。</span>
          </div>
          <p v-else class="m-0 text-cp-xs text-cp-text-secondary">
            当前服务未提供捕获规则，请刷新后查看。
          </p>
          <p class="m-0 text-cp-sm text-cp-text-secondary">
            单次固定最多一小时；到期、服务重启或令牌更换后重新捕获。跨轮复用效果未验证，state 长度不代表模型质量恢复。
          </p>
          <template v-if="pinTurnState">
            <p v-if="recaptureTurnState" role="status" class="m-0 text-cp-sm text-cp-primary-text">
              保存后清除旧绑定，等待符合当前规则的新候选
            </p>
            <p v-else-if="!turnStatePins.length" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
              等待捕获；使用普通请求产生候选，连接测试不会捕获
            </p>
            <ul v-else class="m-0 grid gap-2 pl-4 text-cp-sm text-cp-text">
              <li v-for="(pin, index) in turnStatePins" :key="`${pin.model}-${pin.capturedAt}-${index}`">
                {{ pin.model }} · {{ pin.length }} 字节 · 命中 {{ pin.hits }} 次 · {{ new Date(pin.expiresAt).toLocaleString() }} 到期
              </li>
            </ul>
            <div>
              <BaseButton variant="secondary" :disabled="saving || recaptureTurnState" @click="recaptureTurnState = true">
                重新捕获（保存后生效）
              </BaseButton>
            </div>
          </template>
        </template>
        <AccountTurnStateHistory v-if="showStateHistory" :id="historyId" :account-id="account.id" :capture-rule="turnStateCaptureRule" @close="showStateHistory = false" />
      </section>

      <AccountSettingsFields
        v-model:enabled="enabled"
        v-model:concurrency-limit="concurrencyLimit"
        v-model:weight="weight"
        v-model:model-access="modelAccess"
        v-model:selected-group-ids="selectedGroupIds"
        v-model:proxy-mode="proxyMode"
        v-model:proxy-id="proxyId"
        :groups="groups"
        :groups-loading="groupsLoading"
        :disabled="saving"
        :endpoint="account.outboundProxyEndpoint"
        :account-id="account.id"
      />

      <BaseFormItem label="备注">
        <BaseTextarea
          v-model="notes"
          :rows="3"
          :maxlength="500"
          placeholder="最多 500 字，留空可清除备注。"
          :disabled="saving"
        />
      </BaseFormItem>
    </div>

    <template #footer>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!account || groupsLoading || (account.authenticationKind === 'api_key' && !configurationReady)"
        @click="emit('save')"
      >
        保存更改
      </BaseButton>
    </template>
  </BaseModal>
</template>
