import { fileURLToPath, URL } from 'node:url'
import CodexProxyUI from '@codex-proxy/ui/vite'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

export default defineConfig(({ mode }) => {
  const sourceUi = mode === 'source'
  const uiRoot = new URL('../modules/ui/', import.meta.url)

  return {
    base: '/',
    plugins: [vue(), tailwindcss(), CodexProxyUI({ source: sourceUi ? uiRoot : undefined })],
    resolve: {
      alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
    },
    server: {
      port: 5173,
      proxy: {
        '/dev': {
          target: 'http://127.0.0.1:8080',
          changeOrigin: true,
          rewrite: path => path.replace(/^\/dev/, ''),
        },
        '/guanlan': {
          target: 'https://zzledu.kdns.fr',
          changeOrigin: true,
          rewrite: path => path.replace(/^\/guanlan/, ''),
        },
      },
    },
    build: {
      outDir: sourceUi ? 'node_modules/.vite/source-dist' : 'dist',
      assetsDir: 'assets/account-controls-20260919',
      chunkSizeWarningLimit: 600,
      rolldownOptions: {
        checks: {
          invalidAnnotation: false,
        },
        output: {
          codeSplitting: {
            groups: [
              {
                name: 'echarts',
                test: /node_modules\/echarts/,
              },
              {
                name: 'zrender',
                test: /node_modules\/zrender/,
              },
            ],
          },
        },
      },
    },
  }
})
