<script setup lang="ts">
import { Upload } from '@lucide/vue'
import { useFileDialog } from '@vueuse/core'
import { onScopeDispose, ref, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'

const props = defineProps<{
  label: string
  placeholder: string
  uploadable: boolean
  disabled: boolean
}>()
const text = defineModel<string>({ required: true })
const fileError = ref('')
const dragDepth = ref(0)
const { open: openFile, onChange } = useFileDialog({ accept: 'application/json,.json', multiple: false, reset: true })

let readVersion = 0
onScopeDispose(() => {
  readVersion += 1
})

watch(() => [props.disabled, props.uploadable], () => {
  readVersion += 1
  dragDepth.value = 0
})

onChange(readFiles)

async function readFiles(files: FileList | null) {
  if (props.disabled || !props.uploadable)
    return
  const file = files?.[0]
  if (!file)
    return
  const version = ++readVersion
  fileError.value = ''
  if (files.length !== 1) {
    fileError.value = '请每次选择或拖入一个 JSON 文件'
    return
  }
  if (!file.name.toLowerCase().endsWith('.json') && file.type !== 'application/json') {
    fileError.value = '请选择或拖入 JSON 格式的账号文件'
    return
  }
  try {
    const contents = await file.text()
    if (version !== readVersion)
      return
    text.value = contents
  }
  catch {
    if (version === readVersion)
      fileError.value = '文件读取失败，请重新选择或拖入'
  }
}

function handleDrag(event: DragEvent) {
  if (!event.dataTransfer?.types.includes('Files'))
    return
  event.preventDefault()
  const allowed = props.uploadable && !props.disabled
  event.dataTransfer.dropEffect = allowed ? 'copy' : 'none'
  if (allowed && event.type === 'dragenter')
    dragDepth.value += 1
}

function handleDragLeave() {
  dragDepth.value = Math.max(0, dragDepth.value - 1)
}

function handleDrop(event: DragEvent) {
  dragDepth.value = 0
  if (!event.dataTransfer?.types.includes('Files'))
    return
  event.preventDefault()
  void readFiles(event.dataTransfer.files)
}

function updateText(value: string) {
  text.value = value
  readVersion += 1
  fileError.value = ''
}
</script>

<template>
  <BaseFormItem
    :label="label"
    required
    :error="fileError || undefined"
    :description="uploadable ? '可将一个 JSON 账号文件拖入下方输入框，或点击上传文件' : undefined"
  >
    <template v-if="uploadable" #extra>
      <BaseButton size="sm" :disabled="disabled" @click="openFile()">
        <template #icon>
          <Upload class="size-3.5" aria-hidden="true" />
        </template>
        上传文件
      </BaseButton>
    </template>
    <div class="relative rounded-cp">
      <BaseTextarea
        :model-value="text"
        :aria-label="label"
        :rows="9"
        :placeholder="placeholder"
        :disabled="disabled"
        @update:model-value="updateText"
        @dragenter="handleDrag"
        @dragover="handleDrag"
        @dragleave="handleDragLeave"
        @drop="handleDrop"
      />
      <div
        v-if="dragDepth > 0"
        class="pointer-events-none absolute inset-0 flex items-center justify-center gap-2 rounded-cp bg-cp-primary-container text-cp-primary-on-container ring-2 ring-cp-primary-border"
        role="status"
      >
        <Upload class="size-5" aria-hidden="true" />
        <span>松开以读取账号文件</span>
      </div>
    </div>
  </BaseFormItem>
</template>
