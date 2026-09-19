<script setup lang="ts">
import type { ImportAccount } from './utils/document'
import type { PublicImportEntry } from '@/api'
import { CircleCheck, CircleX, LoaderCircle, Upload } from '@lucide/vue'
import { computed, onMounted, ref, shallowRef } from 'vue'
import { useRoute } from 'vue-router'
import { getPublicImportEntry, submitPublicImport } from '@/api'
import { ApiError } from '@/api/request'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseCard from '@/components/base/BaseCard.vue'
import BaseScrollbar from '@/components/base/BaseScrollbar.vue'
import { formatDateTime } from '@/utils/date'
import AccountImportFields from '@/views/accounts/components/AccountCreateModal/AccountImportFields.vue'
import { importAccountLabel, splitImportDocument } from './utils/document'

interface ImportRow {
  label: string
  status: 'pending' | 'running' | 'imported' | 'failed'
  detail: string
}

// 单账号导入可能包含 RT 换取 AT，放宽到远高于管理端默认的 30 秒。
const IMPORT_TIMEOUT_MS = 180_000
// 与服务端后台导入的执行槽位数一致，避免一个页面占满上游刷新通道。
const CONCURRENCY = 3

const route = useRoute()
const token = String(route.params.token ?? '')
const entry = shallowRef<PublicImportEntry | null>(null)
const entryState = shallowRef<'loading' | 'ready' | 'invalid'>('loading')
const text = ref('')
const parseError = shallowRef('')
const rows = ref<ImportRow[]>([])
const running = shallowRef(false)

const finished = computed(() => rows.value.filter(row => row.status === 'imported' || row.status === 'failed').length)
const imported = computed(() => rows.value.filter(row => row.status === 'imported').length)
const failed = computed(() => rows.value.filter(row => row.status === 'failed').length)

onMounted(async () => {
  try {
    entry.value = await getPublicImportEntry(token, { silent: true })
    entryState.value = 'ready'
  }
  catch {
    entryState.value = 'invalid'
  }
})

async function importOne(account: ImportAccount, row: ImportRow) {
  row.status = 'running'
  try {
    const result = await submitPublicImport(token, { accounts: [account] }, { silent: true, timeout: IMPORT_TIMEOUT_MS })
    const item = result.items[0]
    if (item?.status === 'imported') {
      row.status = 'imported'
      row.detail = [
        item.proxyName ? `出口 ${item.proxyName}` : '',
        entry.value?.pinTurnState ? (item.statePinned ? 'state 绑定已开启' : 'state 绑定未开启') : '',
      ].filter(Boolean).join(' · ')
    }
    else {
      row.status = 'failed'
      row.detail = item?.message ?? '导入失败'
    }
  }
  catch (error) {
    row.status = 'failed'
    row.detail = error instanceof Error ? error.message : '导入失败'
    // 导入途中链接到期或被关闭：后续账号不必再逐个撞 404。
    if (error instanceof ApiError && error.status === 404)
      entryState.value = 'invalid'
  }
}

async function startImport() {
  if (running.value || !entry.value)
    return
  parseError.value = ''
  let accounts: ImportAccount[]
  try {
    accounts = splitImportDocument(text.value, entry.value.maxAccounts)
  }
  catch (error) {
    parseError.value = error instanceof Error ? error.message : '文件无法解析'
    return
  }
  rows.value = accounts.map((account, index) => ({ label: importAccountLabel(account, index), status: 'pending', detail: '' }))
  running.value = true
  let cursor = 0
  // 固定数量的 worker 依次领取账号；单个失败只影响所在行。
  await Promise.all(Array.from({ length: Math.min(CONCURRENCY, accounts.length) }, async () => {
    while (cursor < accounts.length && entryState.value === 'ready') {
      const index = cursor++
      await importOne(accounts[index]!, rows.value[index]!)
    }
  }))
  // 链接中途失效时，没来得及提交的账号明确标成未导入。
  for (const row of rows.value) {
    if (row.status === 'pending') {
      row.status = 'failed'
      row.detail = '链接已失效，未导入'
    }
  }
  running.value = false
  if (failed.value === 0)
    text.value = ''
}
</script>

<template>
  <main class="h-dvh overflow-hidden bg-cp-bg-layout text-cp-text">
    <BaseScrollbar>
      <div class="mx-auto flex min-h-full w-full max-w-3xl flex-col gap-5 p-4 min-[961px]:p-6">
        <header>
          <h1 class="m-0 text-cp-xl font-bold">
            账号导入
          </h1>
          <p class="mt-1.5 mb-0 text-cp-sm text-cp-text-secondary">
            粘贴或上传 sub2api 导出的账号 JSON，提交后自动分配出口并加入指定分组
          </p>
        </header>

        <p v-if="entryState === 'loading'" class="m-0 text-cp-sm text-cp-text-secondary">
          正在校验链接…
        </p>
        <p v-if="entryState === 'invalid'" role="alert" class="m-0 rounded-cp-lg bg-cp-error-container px-4 py-3 text-cp-sm text-cp-error-text">
          导入链接无效或已关闭，请向管理员索取新的链接
        </p>

        <template v-if="entry">
          <BaseCard title="导入设置" description="以下设置由管理员固定，提交的文件无法更改">
            <dl class="m-0 grid gap-2 text-cp-sm">
              <div class="flex gap-3">
                <dt class="w-20 shrink-0 text-cp-text-secondary">
                  目标分组
                </dt>
                <dd class="m-0">
                  {{ entry.groupNames.join('、') }}
                </dd>
              </div>
              <div class="flex gap-3">
                <dt class="w-20 shrink-0 text-cp-text-secondary">
                  出站代理
                </dt>
                <dd class="m-0">
                  每个账号随机分配一条已通过测试的出口，文件自带的代理会被忽略
                </dd>
              </div>
              <div class="flex gap-3">
                <dt class="w-20 shrink-0 text-cp-text-secondary">
                  state 绑定
                </dt>
                <dd class="m-0">
                  {{ entry.pinTurnState ? '导入后自动开启' : '不开启' }}
                </dd>
              </div>
              <div class="flex gap-3">
                <dt class="w-20 shrink-0 text-cp-text-secondary">
                  链接有效期
                </dt>
                <dd class="m-0">
                  {{ entry.expiresAt ? `${formatDateTime(entry.expiresAt)} 前有效` : '长期有效' }}
                </dd>
              </div>
            </dl>
          </BaseCard>

          <BaseCard title="账号文件">
            <div class="grid gap-4">
              <AccountImportFields
                v-model="text"
                label="sub2api 账号 JSON"
                :placeholder="`{ &quot;accounts&quot;: [ … ], &quot;proxies&quot;: [] }，单次最多 ${entry.maxAccounts} 个账号`"
                uploadable
                :disabled="running"
              />
              <p v-if="parseError" role="alert" class="m-0 text-cp-sm text-cp-error">
                {{ parseError }}
              </p>
              <div class="flex items-center justify-end">
                <BaseButton variant="primary" :loading="running" :disabled="!text.trim() || entryState !== 'ready'" @click="startImport">
                  <template #icon>
                    <Upload class="size-4" />
                  </template>
                  {{ running ? `导入中 ${finished}/${rows.length}` : '开始导入' }}
                </BaseButton>
              </div>
            </div>
          </BaseCard>

          <BaseCard v-if="rows.length" title="导入结果" :description="`成功 ${imported} · 失败 ${failed} · 共 ${rows.length}`">
            <ul class="m-0 grid list-none gap-1.5 p-0" aria-live="polite">
              <li
                v-for="(row, index) in rows"
                :key="index"
                class="flex min-w-0 items-start gap-3 rounded-cp bg-cp-fill-quaternary px-3 py-2 text-cp-sm"
              >
                <CircleCheck v-if="row.status === 'imported'" class="mt-0.5 size-4 shrink-0 text-cp-success" aria-label="成功" />
                <CircleX v-else-if="row.status === 'failed'" class="mt-0.5 size-4 shrink-0 text-cp-error" aria-label="失败" />
                <LoaderCircle v-else class="mt-0.5 size-4 shrink-0 text-cp-text-tertiary" :class="{ 'animate-spin': row.status === 'running' }" aria-label="等待中" />
                <div class="min-w-0">
                  <p class="m-0 truncate font-emphasis">
                    {{ row.label }}
                  </p>
                  <p v-if="row.detail" class="mt-0.5 mb-0 break-words" :class="row.status === 'failed' ? 'text-cp-error-text' : 'text-cp-text-secondary'">
                    {{ row.detail }}
                  </p>
                </div>
              </li>
            </ul>
          </BaseCard>
        </template>
      </div>
    </BaseScrollbar>
  </main>
</template>
