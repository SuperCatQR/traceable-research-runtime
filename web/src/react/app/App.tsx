import { LoaderCircle, RefreshCw } from "lucide-react";
import { Navigate, Route, Routes } from "react-router-dom";
import { useSessionQuery } from "../data/workspace-queries";
import { AuthPage } from "../features/auth/AuthPage";
import { ResearchWorkspace } from "../features/research/ResearchWorkspace";

function BootScreen() {
  return (
    <main className="boot-screen" aria-label="正在恢复工作区">
      <span className="brand-mark" aria-hidden="true" />
      <LoaderCircle className="spin" aria-hidden="true" />
    </main>
  );
}

function BootFailure({ onRetry }: { onRetry: () => void }) {
  return (
    <main className="boot-screen boot-failure" role="alert">
      <span className="brand-mark" aria-hidden="true" />
      <h1>无法恢复工作区</h1>
      <p>检查网络连接后再试。</p>
      <button className="secondary-command" type="button" onClick={onRetry}>
        <RefreshCw aria-hidden="true" />重新连接
      </button>
    </main>
  );
}

function AuthenticatedRoutes() {
  return (
    <Routes>
      <Route path="/research" element={<ResearchWorkspace />} />
      <Route path="/research/:conversationId" element={<ResearchWorkspace />} />
      <Route path="/research/archived" element={<ResearchWorkspace />} />
      <Route path="/settings/models" element={<ResearchWorkspace />} />
      <Route path="/login" element={<Navigate to="/research" replace />} />
      <Route path="/register" element={<Navigate to="/research" replace />} />
      <Route path="*" element={<Navigate to="/research" replace />} />
    </Routes>
  );
}

export function App() {
  const session = useSessionQuery();
  if (session.isPending) return <BootScreen />;
  if (session.isError) return <BootFailure onRetry={() => void session.refetch()} />;
  if (!session.data) {
    return (
      <Routes>
        <Route path="/register" element={<AuthPage />} />
        <Route path="*" element={<AuthPage />} />
      </Routes>
    );
  }
  return <AuthenticatedRoutes />;
}
