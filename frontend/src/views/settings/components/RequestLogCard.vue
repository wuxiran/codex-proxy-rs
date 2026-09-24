<script setup lang="ts">
import { computed } from 'vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseForm from '@/components/base/BaseForm/index.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'

defineProps<{ disabled: boolean }>()
const enabled = defineModel<boolean>('enabled', { required: true })
const testKeyId = defineModel<string | null>('testKeyId', { required: true })

// BaseInput 只吃 string；null 与空串互转（空即视为未配置）。
const testKeyText = computed({
  get: () => testKeyId.value ?? '',
  set: (value: string) => {
    testKeyId.value = value.trim() ? value : null
  },
})
</script>

<template>
  <BaseCard
    title="请求日志采集"
    description="只采集我们自己的测试来源流量（指定测试 Client Key）；客户流量不采集。关闭则任何来源都不新增诊断日志。">
    <template #body>
      <div>
        <BaseSwitch v-model="enabled" label="启用请求日志采集" show-label :disabled="disabled" />
      </div>
      <BaseForm class="mt-4 max-w-2xl">
        <label class="block text-xs text-cp-text-secondary">测试 Client Key ID（只有它的流量会被采集；留空则不采集任何客户端流量）</label>
        <BaseInput
          v-model="testKeyText"
          class="mt-1.5"
          placeholder="粘贴测试 Client Key 的 id（如 ck_...）"
          :disabled="disabled || !enabled" />
      </BaseForm>
    </template>
  </BaseCard>
</template>
