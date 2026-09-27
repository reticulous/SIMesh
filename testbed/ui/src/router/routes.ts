import type { RouteRecordRaw } from 'vue-router'

/* One page, so one route — and a catch-all onto it, because the front serves
 * the same document for every path and a deep link has to land somewhere. */
const routes: RouteRecordRaw[] = [
  { path: '/', component: () => import('../layouts/MainLayout.vue') },
  { path: '/:catchAll(.*)*', redirect: '/' },
]

export default routes
