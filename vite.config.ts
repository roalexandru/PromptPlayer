import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));

const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  resolve: {
    alias: {
      $lib: resolve(__dirname, "src/lib"),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? { protocol: "ws", host, port: 1421 }
      : undefined,
    watch: { ignored: ["**/src-tauri/**", "**/guest-helper/**"] },
  },
  build: {
    target: ["es2022", "chrome105", "safari14"],
    sourcemap: !!process.env.TAURI_DEBUG,
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    rollupOptions: {
      input: {
        library: resolve(__dirname, "index.html"),
        picker: resolve(__dirname, "picker.html"),
        settings: resolve(__dirname, "settings.html"),
        "tray-popup": resolve(__dirname, "tray-popup.html"),
      },
    },
  },
});
