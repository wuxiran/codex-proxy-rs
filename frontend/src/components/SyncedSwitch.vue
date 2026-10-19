<script setup lang="ts">
import { ZSwitch } from '@codex-proxy/ui'
import { computed, nextTick, useTemplateRef } from 'vue'

const props = defineProps<{
  label: string
  disabled?: boolean
  /** 为 true 时把 label 渲染成可见文字；否则只作为无障碍名称。 */
  showLabel?: boolean
  activeText?: string
  inactiveText?: string
  inlinePrompt?: boolean
  width?: string | number
}>()

const model = defineModel<boolean>({ default: false })
const root = useTemplateRef<HTMLElement>('root')

// ZSwitch 的 label 直接渲染成文字；fork 调用方沿用 showLabel 决定是否可见。
const switchProps = computed(() => ({
  disabled: props.disabled,
  activeText: props.activeText,
  inactiveText: props.inactiveText,
  inlinePrompt: props.inlinePrompt,
  width: props.width,
  ...(props.showLabel ? { label: props.label } : { 'aria-label': props.label }),
}))

/**
 * fork：受控开关可能等待异步保存；父级未接受新值时，原生 checkbox 也应回显当前值。
 * 共享 UI 包的 ZSwitch 不处理这一点，这里在父级决定后回写内部的原生控件。
 */
async function syncChecked() {
  await nextTick()
  const input = root.value?.querySelector<HTMLInputElement>('input[type="checkbox"]')
  if (input)
    input.checked = model.value
}
</script>

<template>
  <span ref="root" class="contents">
    <ZSwitch v-bind="switchProps" v-model="model" @change="syncChecked" />
  </span>
</template>
