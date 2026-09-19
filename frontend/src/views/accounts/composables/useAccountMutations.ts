import type { Ref } from 'vue'
import type { AccountImportTask, getAccounts } from '@/api'
import type { RequestOptions } from '@/api/request'
import dayjs from 'dayjs'
import { ref, watch } from 'vue'
import {
  batchUpdateAccounts,
  deleteAccounts,
  exportAccounts,
  getAccountDetail,
  recoverAccount,
  refreshAccount,
  refreshAccountQuota,
  reviveGuanlanAccount,
  updateAccountTurnState,
} from '@/api'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { useDownload } from '@/composables/useDownload'
import { useIdSet } from '@/composables/useIdSet'
import { errorMessage, withMinimumDuration } from '@/utils/async'

import { useAccountOnboarding } from './useAccountOnboarding'

type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]

export function useAccountMutations(options: {
  accounts: Ref<AccountRow[]>
  selectedIds: Ref<Set<string>>
  onImportTaskCreated: (task: AccountImportTask) => void
  reload: () => Promise<unknown>
  replaceAccount: (account: AccountRow) => Promise<boolean>
  reloadConfigurations: () => Promise<void>
}) {
  const loadAccounts = options.reload
  const { downloadJson } = useDownload()
  const onboarding = useAccountOnboarding({
    reload: loadAccounts,
    onImportTaskCreated: options.onImportTaskCreated,
  })
  const selectedAccountsById = new Map<string, AccountRow>()
  const showDeleteModal = ref(false)
  const showSingleDeleteModal = ref(false)
  const pendingDeleteAccount = ref<AccountRow | null>(null)
  const recoveringAccounts = useIdSet<string>()
  const refreshingAccounts = useIdSet<string>()
  const refreshingQuotaAccounts = useIdSet<string>()
  const updatingSchedulingAccounts = useIdSet<string>()
  const updatingTurnStateAccounts = useIdSet<string>()
  const revivingAccounts = useIdSet<string>()
  const deletingAccountAction = useAsyncAction()
  const batchDeletingAction = useAsyncAction()
  const exportingAccountsAction = useAsyncAction()
  const recoveringAccountIds = recoveringAccounts.ids
  const refreshingAccountIds = refreshingAccounts.ids
  const refreshingQuotaAccountIds = refreshingQuotaAccounts.ids
  const updatingSchedulingAccountIds = updatingSchedulingAccounts.ids
  const updatingTurnStateAccountIds = updatingTurnStateAccounts.ids
  const revivingAccountIds = revivingAccounts.ids
  const deletingAccount = deletingAccountAction.loading
  const batchDeleting = batchDeletingAction.loading
  const exportingAccounts = exportingAccountsAction.loading

  watch(
    [options.accounts, options.selectedIds],
    ([accounts, selectedIds]) => {
      for (const account of accounts) {
        if (selectedIds.has(account.id))
          selectedAccountsById.set(account.id, account)
      }
      for (const accountId of selectedAccountsById.keys()) {
        if (!selectedIds.has(accountId))
          selectedAccountsById.delete(accountId)
      }
    },
    { immediate: true, flush: 'sync' },
  )

  function requestDeleteAccount(account: AccountRow) {
    pendingDeleteAccount.value = account
    showSingleDeleteModal.value = true
  }

  async function handleDelete() {
    const account = pendingDeleteAccount.value
    if (deletingAccount.value || !account)
      return

    await deletingAccountAction.run(
      async () => {
        await deleteAccountBatch([account])
        const remaining = new Set(options.selectedIds.value)
        remaining.delete(account.id)
        options.selectedIds.value = remaining
        showSingleDeleteModal.value = false
        pendingDeleteAccount.value = null
        await loadAccounts()
        toast.success('账号已删除')
      },
    )
  }

  async function handleBatchDelete() {
    if (batchDeleting.value || options.selectedIds.value.size === 0)
      return

    let deletedCount = 0
    await batchDeletingAction.run(
      async () => {
        const selected = accountsById([...options.selectedIds.value])
        for (const accounts of accountDeletionGroups(selected)) {
          await deleteAccountBatch(accounts, { silent: true })
          deletedCount += accounts.length
          const deletedIds = new Set(accounts.map(account => account.id))
          const remaining = new Set(options.selectedIds.value)
          for (const accountId of deletedIds)
            remaining.delete(accountId)
          options.selectedIds.value = remaining
        }
        showDeleteModal.value = false
        await loadAccounts()
        toast.success(`已删除 ${deletedCount} 个账号`)
      },
      {
        errorText: false,
        onError: (error) => {
          void loadAccounts().catch(() => undefined)
          toast.error(
            deletedCount > 0
              ? `已删除 ${deletedCount} 个账号，其余未删除：${errorMessage(error, '操作失败')}`
              : errorMessage(error, '批量删除失败'),
          )
        },
      },
    )
  }

  async function handleExportAccounts() {
    if (exportingAccounts.value)
      return
    const selected = [...options.selectedIds.value]
    if (selected.length === 0) {
      toast.warning('请选择要导出的账号')
      return
    }

    await exportingAccountsAction.run(
      async () => {
        const payload = await exportAccounts({
          accountIds: selected.join(','),
          confirm: 'export_sensitive_accounts',
        })
        const fileName = `cpr-accounts-selected-${selected.length}-${dayjs().format('YYYY-MM-DD')}.json`
        await downloadJson(payload, fileName)
        toast.success(`已导出 ${selected.length} 个账号`)
      },
      { errorText: '导出失败' },
    )
  }

  async function handleRefresh(accountId: string) {
    await refreshingAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(() =>
          refreshAccount({
            accountId,
          }),
        )
        await loadAccounts()
        if (result.result === 'skipped') {
          toast.warning(result.error || 'Token 正在刷新中')
          return
        }
        if (result.result === 'failed') {
          toast.error(result.error || '刷新失败')
          return
        }
        toast.success('Token 已刷新')
      }
      catch {}
    })
  }

  async function handleRefreshQuota(accountId: string) {
    await refreshingQuotaAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(() => refreshAccountQuota({ accountId }))
        const remainsVisible = await options.replaceAccount(result.account)
        if (!remainsVisible) {
          const selectedIds = new Set(options.selectedIds.value)
          selectedIds.delete(accountId)
          options.selectedIds.value = selectedIds
        }
        toast.success('额度已刷新')
      }
      catch {}
    })
  }

  async function handleToggleTurnState(accountId: string, pinTurnState: boolean) {
    await updatingTurnStateAccounts.run(accountId, async () => {
      try {
        await updateAccountTurnState({ accountId, pinTurnState })
        toast.success(pinTurnState ? '已开启 state 绑定' : '已关闭 state 绑定')
        await options.reloadConfigurations()
      }
      catch {}
    })
  }

  async function handleReviveGuanlan(accountId: string) {
    await revivingAccounts.run(accountId, async () => {
      try {
        await reviveGuanlanAccount({ accountId })
        toast.success('guanlan 复活成功')
        await loadAccounts()
      }
      catch {}
    })
  }

  async function handleToggleScheduling(account: AccountRow, enabled: boolean) {
    await updatingSchedulingAccounts.run(account.id, async () => {
      try {
        // 局部更新只提交调度字段，避免覆盖其他管理员刚修改的账号配置。
        await batchUpdateAccounts({ accountIds: [account.id], enabled })
      }
      catch {
        return
      }

      toast.success(enabled ? '已开启调度' : '已关闭调度')
      try {
        const result = await getAccountDetail({ accountId: account.id })
        const remainsVisible = await options.replaceAccount(result.account)
        if (!remainsVisible) {
          const selectedIds = new Set(options.selectedIds.value)
          selectedIds.delete(account.id)
          options.selectedIds.value = selectedIds
        }
      }
      catch {
        await loadAccounts()
      }
    })
  }

  async function handleRecover(accountId: string) {
    await recoveringAccounts.run(accountId, async () => {
      try {
        const result = await withMinimumDuration(() => recoverAccount({ accountId }))
        const remainsVisible = await options.replaceAccount(result.account)
        if (!remainsVisible) {
          const selectedIds = new Set(options.selectedIds.value)
          selectedIds.delete(accountId)
          options.selectedIds.value = selectedIds
        }
        toast.success('账号状态已恢复')
      }
      catch {}
    })
  }

  function accountsById(ids: string[]) {
    const accounts = []
    for (const id of ids) {
      const account = selectedAccountsById.get(id)
      if (!account)
        throw new Error(`账号 ${id} 的页面数据已失效，请重新选择`)
      accounts.push(account)
    }
    return accounts
  }

  async function deleteAccountBatch(accounts: AccountRow[], options?: RequestOptions) {
    const account = accounts[0]
    if (!account)
      return
    const payload = {
      provider: account.provider,
      accountIds: accounts.map(account => account.id),
    }
    const result = await deleteAccounts(payload, options)
    if (!result)
      throw new Error(`不支持的 Provider：${account.provider}`)
  }

  function accountDeletionGroups(accounts: AccountRow[]) {
    const groups = new Map<string, AccountRow[]>()
    for (const account of accounts) {
      const key = account.provider
      const group = groups.get(key)
      if (group)
        group.push(account)
      else
        groups.set(key, [account])
    }
    return groups.values()
  }

  return {
    ...onboarding,
    showDeleteModal,
    showSingleDeleteModal,
    pendingDeleteAccount,
    recoveringAccountIds,
    refreshingAccountIds,
    refreshingQuotaAccountIds,
    updatingSchedulingAccountIds,
    updatingTurnStateAccountIds,
    revivingAccountIds,
    deletingAccount,
    batchDeleting,
    exportingAccounts,
    requestDeleteAccount,
    handleDelete,
    handleBatchDelete,
    handleExportAccounts,
    handleRecover,
    handleRefresh,
    handleRefreshQuota,
    handleToggleScheduling,
    handleToggleTurnState,
    handleReviveGuanlan,
  }
}
