<script setup lang="ts">
import { BaseSwitch } from '@codex-proxy/ui'
import { nextTick } from 'vue'

const props = defineProps<{
  label: string
  disabled?: boolean
  showLabel?: boolean
  activeText?: string
  inactiveText?: string
  inlinePrompt?: boolean
  width?: string | number
}>()

const model = defineModel<boolean>({ default: false })

/**
 * fork：受控开关可能等待异步保存；父级未接受新值时，原生 checkbox 也应回显当前值。
 * 共享 UI 包的 BaseSwitch 不处理这一点；原生 change 会冒泡到它的根元素，这里在父级决定后回写。
 */
async function syncChecked(event: Event) {
  const input = event.target
  if (!(input instanceof HTMLInputElement))
    return
  await nextTick()
  input.checked = model.value
}
</script>

<template>
  <BaseSwitch v-bind="props" v-model="model" @change="syncChecked" />
</template>
