import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tanstackRouter from "@tanstack/router-plugin/vite";
import path from "node:path";

export default defineConfig({
  plugins: [tanstackRouter({ autoCodeSplitting: true }), react()],
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },
  server: {
    port: 5173,
    proxy: {
      "/api": {
        target: process.env.SETU_API_URL ?? "http://127.0.0.1:7000",
        // Keep the browser's Host header: the API's CSRF check compares
        // Origin against Host, and rewriting Host made every mutation in
        // dev a 403.
        changeOrigin: false,
      },
    },
  },
});
