import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";
import type { ReactNode } from "react";
import { WorkspaceGatewayProvider, type WorkspaceGateway } from "../data/workspace-gateway";

export function AppProviders({
  gateway,
  children,
  queryClient,
}: {
  gateway: WorkspaceGateway;
  children: ReactNode;
  queryClient: QueryClient;
}) {
  return (
    <BrowserRouter>
      <QueryClientProvider client={queryClient}>
        <WorkspaceGatewayProvider gateway={gateway}>{children}</WorkspaceGatewayProvider>
      </QueryClientProvider>
    </BrowserRouter>
  );
}
