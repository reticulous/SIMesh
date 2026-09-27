import { configure } from 'quasar/wrappers';

// Where `quasar dev` sends /ws, /api and /planner: the front, run beside it
// (`npm run dev` in this directory while `simesh` runs).
const FRONT = process.env.SIMESH_FRONT || 'http://127.0.0.1:8800';

export default configure(() => {
  return {
    boot: [],
    css: ['app.css'],
    extras: [],
    build: {
      target: { browser: ['es2022'] },
      vueRouterMode: 'history',
      extendViteConf(viteConf) {
        // planner-wasm finds its .wasm beside its own module; pre-bundled, it
        // would look for it in Vite's cache instead. The page passes the URL
        // explicitly as well, so this only keeps `quasar dev` honest.
        viteConf.optimizeDeps = {
          ...viteConf.optimizeDeps,
          exclude: [...(viteConf.optimizeDeps?.exclude ?? []), 'planner-wasm'],
        };
        if (viteConf.build) {
          viteConf.build.chunkSizeWarningLimit = Infinity;
        }
      },
    },
    devServer: {
      open: false,
      // Same-origin with the front, so the websocket, the sidecar and the
      // station links behave exactly as when the front serves the built page.
      proxy: {
        '/ws': { target: FRONT, changeOrigin: true, ws: true },
        '/api': { target: FRONT, changeOrigin: true },
        '/planner': { target: FRONT, changeOrigin: true },
      },
    },
    framework: {
      iconSet: 'svg-material-icons',
      config: { dark: true },
      plugins: ['Dialog', 'Notify'],
    },
  };
});
