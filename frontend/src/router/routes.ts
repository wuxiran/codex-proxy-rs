import type { RouteRecordRaw } from 'vue-router'

export const routes: RouteRecordRaw[] = [
  {
    path: '/login',
    name: 'login',
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
    component: () => import('@/views/key-usage/index.vue'),
  },
  {
    path: '/',
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
        path: 'theme',
        name: 'theme',
        component: () => import('@/views/theme/index.vue'),
      },
      {
        path: 'settings',
        name: 'settings',
        component: () => import('@/views/settings/index.vue'),
      },
      {
        path: 'settings/upstream',
        name: 'settings-upstream',
        component: () => import('@/views/settings/index.vue'),
      },
      {
        path: 'settings/access',
        name: 'settings-access',
        component: () => import('@/views/settings/index.vue'),
      },
      {
        path: 'settings/backup',
        name: 'settings-backup',
        component: () => import('@/views/settings/index.vue'),
      },
      {
        path: 'settings/pricing',
        name: 'settings-pricing',
        component: () => import('@/views/settings/index.vue'),
      },
    ],
  },
  {
    path: '/:pathMatch(.*)*',
    redirect: '/',
  },
]
