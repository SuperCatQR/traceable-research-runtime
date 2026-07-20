import { QueryClient } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "../styles.css";
import "../demo.css";
import "./react-overrides.css";
import { App } from "./app/App";
import { AppProviders } from "./app/providers";
import { httpWorkspaceGateway, type WorkspaceGateway } from "./data/workspace-gateway";

const root = document.querySelector<HTMLElement>("#app");
if (!root) throw new Error("Missing #app");

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: 1,
      staleTime: 10_000,
      gcTime: 30 * 60_000,
    },
    mutations: { retry: false },
  },
});
let gateway: WorkspaceGateway = httpWorkspaceGateway;
const searchParams = new URLSearchParams(window.location.search);
const demoScenario = import.meta.env.DEV ? searchParams.get("demo") : null;
if (demoScenario) {
  const { createDemoWorkspaceGateway, resolveDemoWorkspaceScenario } = await import(
    "./test/demo-workspace-gateway"
  );
  gateway = createDemoWorkspaceGateway(resolveDemoWorkspaceScenario(demoScenario));
}

createRoot(root).render(
  <StrictMode>
    <AppProviders gateway={gateway} queryClient={queryClient}>
      <App />
    </AppProviders>
  </StrictMode>,
);
