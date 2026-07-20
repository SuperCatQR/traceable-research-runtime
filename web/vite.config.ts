import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv } from "vite";
import { selectApiProxyTarget } from "./src/vite-api-proxy";

export default defineConfig(({ mode }) => {
  const fileEnvironment = loadEnv(mode, process.cwd(), "");
  const apiProxyTarget = selectApiProxyTarget(
    process.env.VITE_API_PROXY_TARGET,
    fileEnvironment.VITE_API_PROXY_TARGET,
  );

  return {
    plugins: [react()],
    server: {
      proxy: {
        "/api": {
          target: apiProxyTarget,
          changeOrigin: true,
          headers: { origin: new URL(apiProxyTarget).origin },
        },
      },
    },
    build: {
      rollupOptions: {
        output: {
          manualChunks: {
            markdown: ["react-markdown", "remark-gfm"],
          },
        },
      },
    },
    test: {
      environment: "jsdom",
      setupFiles: ["./src/react/test/setup.ts"],
      include: ["src/react/**/*.test.{ts,tsx}"],
      css: false,
    },
  };
});
