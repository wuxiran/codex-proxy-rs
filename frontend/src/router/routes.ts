import type { RouteRecordRaw } from 'vue-router'
import type { AuthSession } from '@/api'

declare module 'vue-router' {
  interface RouteMeta {
    role?: AuthSession['role']
    defaultEntry?: boolean
    guestOnly?: boolean
  }
}

export const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
    meta: { guestOnly: true },
    component: () => import('@/views/login/index.vue'),
  },
  {
    // 免登录密链页面；令牌在路径里，接口调用时改放请求头。
    path: '/import/:token',
    name: 'public-import',
    component: () => import('@/views/public-import/index.vue'),
  },
  {
    path: '/key-usage',
    name: 'key-usage',
    meta: { role: 'key', defaultEntry: true },
    component: () => import('@/views/key-usage/index.vue'),
  },
  {
    path: '/',
    meta: { role: 'admin', defaultEntry: true },
    component: () => import('@/layout/index.vue'),
    children: [
      {
        path: '',
        name: 'dashboard',
        component: () => import('@/views/dashboard/index.vue'),
      },
      {
        path: 'accounts',
        name: 'accounts',
        component: () => import('@/views/accounts/index.vue'),
      },
      {
        // 免登录导入管理页（生成/轮换密链）；密链持有者面向的页面是 /import/:token。
        path: 'public-import',
        name: 'public-import-admin',
        component: () => import('@/views/public-import-admin/index.vue'),
      },
      {
        path: 'proxies',
        name: 'proxies',
        component: () => import('@/views/proxies/index.vue'),
      },
      {
        path: 'groups',
        name: 'groups',
        component: () => import('@/views/groups/index.vue'),
      },
      {
        path: 'keys',
        name: 'keys',
        component: () => import('@/views/keys/index.vue'),
      },
      {
        path: 'usage',
        name: 'usage',
        component: () => import('@/views/usage/index.vue'),
      },
      {
        path: 'ops-report',
        name: 'ops-report',
        component: () => import('@/views/ops-report/index.vue'),
      },
      {
        path: 'logs',
        name: 'logs',
        component: () => import('@/views/logs/index.vue'),
      },
      {
        path: 'testbench',
        name: 'testbench',
        component: () => import('@/views/testbench/index.vue'),
      },
      {
        path: 'turn-state',
        name: 'turn-state',
        component: () => import('@/views/turn-state/index.vue'),
      },
      {
        path: 'plugins',
        component: () => import('@/views/plugins/index.vue'),
        children: [
          {
            path: '',
            name: 'plugins',
            component: () => import('@/views/plugins/components/PluginManagement.vue'),
          },
          {
            path: ':instanceId/:pageId',
            name: 'plugin-page',
            component: () => import('@/views/plugins/components/PluginPage.vue'),
          },
        ],
      },
      {
        path: 'theme',
        name: 'theme',
        component: () => import('@/views/theme/index.vue'),
      },
      {
        path: 'settings',
        children: [
          {
            path: '',
            name: 'settings',
            component: () => import('@/views/settings/index.vue'),
          },
          {
            path: 'upstream',
            name: 'settings-upstream',
            component: () => import('@/views/settings/index.vue'),
          },
          {
            path: 'access',
            name: 'settings-access',
            component: () => import('@/views/settings/index.vue'),
          },
          {
            path: 'backup',
            name: 'settings-backup',
            component: () => import('@/views/settings/index.vue'),
          },
          {
            path: 'pricing',
            name: 'settings-pricing',
            component: () => import('@/views/settings/index.vue'),
          },
        ],
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
