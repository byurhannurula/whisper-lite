import { defineConfig } from "vite";
import { resolve } from "node:path";

// Fixed port so tauri.conf.json's devUrl always matches; strictPort so a stale process fails
// loudly instead of silently serving the app somewhere Tauri isn't looking.
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: "safari15",
    emptyOutDir: true,
    rollupOptions: {
      // One entry per window. The HUD is on the latency path, so it stays its own tiny bundle
      // rather than sharing one with the settings UI.
      input: {
        main: resolve(__dirname, "index.html"),
        hud: resolve(__dirname, "src/hud/index.html"),
        settings: resolve(__dirname, "src/settings/index.html"),
      },
    },
  },
});
