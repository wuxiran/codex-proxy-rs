<script setup lang="ts">
import { Upload } from '@lucide/vue'
import { useFileDialog } from '@vueuse/core'
import { onScopeDispose, ref, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseTextarea from '@/components/base/BaseTextarea.vue'
import { isSupportedProvider } from '@/utils/providers'
import { combineAccountFilesToEnvelope } from '../../utils/importDocuments'

const props = defineProps<{
  label: string
  placeholder: string
  uploadable: boolean
  disabled: boolean
  /** 当前所选账号平台；多文件合并时用来给每份文件归类。缺省（如免登录导入页）则只支持单文件。 */
  provider?: string
  /** 当前导入模式；只有「账号文件」(json) 模式支持一次丢入多个文件。 */
  mode?: string
}>()
const text = defineModel<string>({ required: true })
const fileError = ref('')
const dragDepth = ref(0)
// 「账号文件」模式允许一次丢入多个文件；其它可上传模式仍是单文件。
const { open: openFile, onChange } = useFileDialog({ accept: 'application/json,.json', multiple: true, reset: true })

let readVersion = 0
onScopeDispose(() => {
  readVersion += 1
})

watch(() => [props.disabled, props.uploadable], () => {
  readVersion += 1
  dragDepth.value = 0
})

onChange(readFiles)

function isJsonFile(file: File) {
  return file.name.toLowerCase().endsWith('.json') || file.type === 'application/json'
}

async function readFiles(fileList: FileList | null) {
  if (props.disabled || !props.uploadable)
    return
  const files = fileList ? Array.from(fileList) : []
  if (files.length === 0)
    return
  const version = ++readVersion
  fileError.value = ''
  const invalid = files.find(file => !isJsonFile(file))
  if (invalid) {
    fileError.value = '请选择或拖入 JSON 格式的账号文件'
    return
  }
  // 单文件（或非「账号文件」模式）：原样读入文本框，便于查看/微调。
  if (files.length === 1 || props.mode !== 'json') {
    try {
      const contents = await files[0]!.text()
      if (version === readVersion)
        text.value = contents
    }
    catch {
      if (version === readVersion)
        fileError.value = '文件读取失败，请重新选择或拖入'
    }
    return
  }
  // 多文件「账号文件」：读全部并按所选平台合并成一个 { documents:[...] } 信封。
  const provider = props.provider ?? ''
  if (!isSupportedProvider(provider)) {
    fileError.value = '请先选择账号平台，再一次丢入多个账号文件'
    return
  }
  try {
    const contents = await Promise.all(files.map(async file => ({ name: file.name, text: await file.text() })))
    if (version !== readVersion)
      return
    text.value = combineAccountFilesToEnvelope(provider, contents)
  }
  catch (error) {
    if (version === readVersion)
      fileError.value = error instanceof Error ? error.message : '文件读取失败，请重新选择或拖入'
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
    :description="uploadable ? (mode === 'json' ? '可将一个或多个 JSON 账号文件拖入下方输入框，或点击上传文件（多个文件会合并为一次批量导入）' : '可将一个 JSON 账号文件拖入下方输入框，或点击上传文件') : undefined"
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
