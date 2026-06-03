import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { fileURLToPath } from "node:url";

// Vite config lives in `frontend/` so `root` is this file's directory.
// Lets us call `vite --config frontend/vite.config.ts` from any cwd.
// Output lands at `frontend/dist/`; `backend/tauri.conf.json` points to `../frontend/dist`.
const here = fileURLToPath(new URL(".", import.meta.url));

export default defineConfig({
  root: here,
  plugins: [react()],
  clearScreen: false,
  esbuild: {
    tsconfigRaw: {
      compilerOptions: {
        jsx: "react-jsx",
        target: "ES2022",
      },
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: false,
    watch: {
      ignored: ["**/backend/**"],
    },
  },
  build: {
    outDir: "dist",
    target: "es2022",
    sourcemap: false,
    minify: "esbuild",
    rollupOptions: {
      output: {
        manualChunks: undefined,
      },
    },
  },
});
