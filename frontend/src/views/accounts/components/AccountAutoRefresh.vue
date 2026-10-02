<script setup lang="ts">
import type { SelectOption } from '@/components/base/BaseSelect.vue'
import { RefreshCw } from '@lucide/vue'
import { computed } from 'vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'
import { AUTO_REFRESH_SECONDS } from '../composables/useAccountsQuery'

defineProps<{
  refreshing: boolean
}>()

const emit = defineEmits<{
  refresh: []
}>()

const seconds = defineModel<number>({ required: true })

const options: SelectOption[] = AUTO_REFRESH_SECONDS.map(value => ({
  value: String(value),
  label: value === 0 ? '自动刷新：关' : `自动刷新：${value} 秒`,
}))

const selected = computed({
  get: () => String(seconds.value),
  set: (value: string) => { seconds.value = Number(value) },
})
</script>

<template>
  <div class="flex items-center gap-2">
    <BaseSelect
      v-model="selected"
      class="w-40"
      aria-label="自动刷新间隔"
      :options="options"
    />
    <BaseIconButton
      label="立即刷新"
      variant="filled"
      :loading="refreshing"
      @click="emit('refresh')"
    >
      <template #loading>
        <RefreshCw class="size-4.5 animate-spin motion-reduce:animate-none" aria-hidden="true" />
      </template>
      <RefreshCw class="size-4.5" aria-hidden="true" />
    </BaseIconButton>
  </div>
</template>
