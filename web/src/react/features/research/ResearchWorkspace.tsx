import { CircleAlert, LoaderCircle, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useState,
} from "react";
import { useLocation, useNavigate, useParams } from "react-router-dom";
import {
  type ModelProfile,
  type SaveModelProfileInput,
  type UserAccount,
} from "../../../research-workspace-client";
import { ArchiveManager } from "../archives/ArchiveManager";
import { ModelSettings } from "../models/ModelSettings";
import { ResearchInspector, type InspectorView } from "../trace/ResearchInspector";
import { useWorkspaceActions } from "../../data/workspace-actions";
import {
  useArchivedConversationsQuery,
  useArchivedModelsQuery,
  useConversationQuery,
  useConversationsQuery,
  useModelsQuery,
  useSessionQuery,
} from "../../data/workspace-queries";
import { useTheme } from "../../shared/use-theme";
import { ConversationSidebar } from "./ConversationSidebar";
import { ConversationTranscript } from "./ConversationTranscript";
import { ResearchComposer } from "./ResearchComposer";
import { WorkspaceHeader } from "./WorkspaceHeader";

export function ResearchWorkspace() {
  const session = useSessionQuery();
  const account = session.data as UserAccount;
  const { conversationId } = useParams<{ conversationId: string }>();
  const location = useLocation();
  const navigate = useNavigate();
  const modelsQuery = useModelsQuery(account.user_id);
  const conversationsQuery = useConversationsQuery(account.user_id);
  const conversationQuery = useConversationQuery(account.user_id, conversationId);
  const models = modelsQuery.data ?? [];
  const conversations = conversationsQuery.data ?? [];
  const conversation = conversationQuery.data;
  const modelSettingsOpen = location.pathname === "/settings/models";
  const archivesOpen = location.pathname === "/research/archived";
  const archivedModels = useArchivedModelsQuery(account.user_id, archivesOpen);
  const archivedConversations = useArchivedConversationsQuery(account.user_id, archivesOpen);
  const { theme, toggleTheme } = useTheme();
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [inspectorTurnId, setInspectorTurnId] = useState<string>();
  const [inspectorView, setInspectorView] = useState<InspectorView>("summary");
  const [auditStage, setAuditStage] = useState("");
  const [draft, setDraft] = useState("");
  const [pendingComposer, setPendingComposer] = useState<string | null>(null);

  const handleAuthenticationExpired = useCallback(() => {
    setDraft("");
    setPendingComposer(null);
    navigate("/login", { replace: true });
  }, [navigate]);
  const actions = useWorkspaceActions(account.user_id, handleAuthenticationExpired);

  const returnPath = conversationId ? `/research/${encodeURIComponent(conversationId)}` : "/research";

  useEffect(() => {
    const reason = modelsQuery.error
      ?? conversationsQuery.error
      ?? conversationQuery.error
      ?? archivedModels.error
      ?? archivedConversations.error;
    if (reason) actions.handleError(reason);
  }, [
    archivedConversations.error,
    archivedModels.error,
    conversationQuery.error,
    conversationsQuery.error,
    actions.handleError,
    modelsQuery.error,
  ]);

  useEffect(() => {
    if (!modelsQuery.isSuccess || !conversationsQuery.isSuccess) return;
    if (models.length === 0 && !modelSettingsOpen) {
      navigate("/settings/models", { replace: true });
      return;
    }
    if (!conversationId && !modelSettingsOpen && !archivesOpen && conversations[0]) {
      navigate(`/research/${encodeURIComponent(conversations[0].conversation_id)}`, { replace: true });
    }
  }, [
    archivesOpen,
    conversationId,
    conversations,
    conversationsQuery.isSuccess,
    modelSettingsOpen,
    models.length,
    modelsQuery.isSuccess,
    navigate,
  ]);

  useEffect(() => {
    setInspectorOpen(false);
    setInspectorTurnId(undefined);
    setInspectorView("summary");
    setAuditStage("");
    setSidebarOpen(false);
    setPendingComposer(null);
    const stored = conversationId
      ? window.sessionStorage.getItem(`research-draft:${account.user_id}:${conversationId}`)
      : null;
    setDraft(stored ?? "");
  }, [account.user_id, conversationId]);

  const setAndPersistDraft = (value: string) => {
    setDraft(value);
    if (conversationId) {
      const storageKey = `research-draft:${account.user_id}:${conversationId}`;
      if (value) window.sessionStorage.setItem(storageKey, value);
      else window.sessionStorage.removeItem(storageKey);
    }
  };

  const createConversation = async () => {
    const selected = models.find((model) => model.is_default) ?? models[0];
    if (!selected) {
      navigate("/settings/models");
      return;
    }
    const created = await actions.conversation.create(selected.profile_id);
    if (!created) return;
    navigate(`/research/${encodeURIComponent(created.conversation_id)}`);
  };

  const updateConversation = async (changes: { title?: string; model_profile_id?: string }) => {
    if (!conversation) return false;
    return actions.conversation.update(conversation.conversation_id, changes);
  };

  const archiveConversation = async () => {
    if (!conversation || !window.confirm(`归档“${conversation.title}”？`)) return;
    const archived = await actions.conversation.archive(conversation.conversation_id);
    if (!archived) return;
    navigate("/research", { replace: true });
  };

  const submitComposer = async () => {
    const text = draft.trim();
    if (!conversation || !text) return;
    setPendingComposer(text);
    const succeeded = await actions.conversation.send(conversation, text);
    setPendingComposer(null);
    if (!succeeded) return;
    setAndPersistDraft("");
  };

  const saveModel = async (profileId: string | null, input: SaveModelProfileInput): Promise<boolean> => {
    return actions.model.save(profileId, input);
  };

  const restoreModel = async (profileId: string) => {
    await actions.model.restore(profileId);
  };

  const restoreConversation = async (archivedId: string, modelProfileId?: string) => {
    const restored = await actions.conversation.restore(archivedId, modelProfileId);
    if (!restored) return;
    navigate(`/research/${encodeURIComponent(restored.conversation_id)}`);
  };

  const logout = async () => {
    await actions.logout();
    setDraft("");
    setPendingComposer(null);
    navigate("/login", { replace: true });
  };

  const openInspector = (turnId?: string) => {
    setSidebarOpen(false);
    setInspectorTurnId(turnId ?? conversation?.turns.at(-1)?.turn_id);
    setInspectorOpen(true);
  };

  const initialLoading = modelsQuery.isPending || conversationsQuery.isPending;
  const busy = actions.busy || Boolean(pendingComposer);
  const selectedConversation = conversationId ? conversation : undefined;

  return (
    <main className={`workspace-shell demo-shell${inspectorOpen ? " has-inspector" : ""}`}>
      <ConversationSidebar
        account={account}
        conversations={conversations}
        activeConversationId={conversationId}
        open={sidebarOpen}
        busy={busy}
        onClose={() => setSidebarOpen(false)}
        onCreate={() => void createConversation()}
        onSelect={(id) => navigate(`/research/${encodeURIComponent(id)}`)}
        onOpenArchives={() => navigate("/research/archived")}
        onOpenSettings={() => navigate("/settings/models")}
      />
      {sidebarOpen && <button className="sidebar-scrim mobile-scrim" type="button" onClick={() => setSidebarOpen(false)} aria-label="关闭对话列表" />}
      <section className="research-workspace workspace scroll-indicator-host" aria-label="研究工作区">
        <WorkspaceHeader
          conversation={selectedConversation}
          models={models}
          theme={theme}
          busy={busy}
          onOpenSidebar={() => { setInspectorOpen(false); setSidebarOpen(true); }}
          onRename={(title) => updateConversation({ title })}
          onChangeModel={(profileId) => void updateConversation({ model_profile_id: profileId })}
          onToggleTheme={toggleTheme}
          onToggleInspector={() => inspectorOpen ? setInspectorOpen(false) : openInspector()}
          onOpenSettings={() => navigate("/settings/models")}
          onArchive={() => void archiveConversation()}
        />
        {initialLoading || (conversationId && conversationQuery.isPending) ? (
          <div className="workspace-empty"><LoaderCircle className="spin" aria-hidden="true" /><h2>正在读取研究工作区</h2></div>
        ) : (
          <ConversationTranscript
            conversation={selectedConversation}
            hasModels={models.length > 0}
            pendingMessage={pendingComposer ?? undefined}
            onCreateConversation={() => void createConversation()}
            onOpenSettings={() => navigate("/settings/models")}
            onOpenInspector={openInspector}
          />
        )}
        <ResearchComposer
          conversation={selectedConversation}
          draft={draft}
          pending={Boolean(pendingComposer)}
          onDraftChange={setAndPersistDraft}
          onSubmit={() => void submitComposer()}
        />
      </section>
      {selectedConversation && (
        <ResearchInspector
          accountId={account.user_id}
          conversation={selectedConversation}
          open={inspectorOpen}
          selectedTurnId={inspectorTurnId}
          view={inspectorView}
          stage={auditStage}
          onClose={() => setInspectorOpen(false)}
          onSelectTurn={setInspectorTurnId}
          onChangeView={setInspectorView}
          onChangeStage={setAuditStage}
          onAuthenticationError={actions.handleError}
        />
      )}
      <ModelSettings
        account={account}
        models={models}
        open={modelSettingsOpen}
        busy={busy}
        error={modelSettingsOpen ? actions.error : null}
        onClose={() => navigate(returnPath)}
        onClearError={actions.clearError}
        onSave={saveModel}
        onVerify={(profileId) => void actions.model.verify(profileId)}
        onSetDefault={(profileId) => void actions.model.setDefault(profileId)}
        onArchive={(profileId) => {
          const profile = models.find((candidate) => candidate.profile_id === profileId);
          if (profile && window.confirm(`归档模型配置“${profile.display_name}”？`)) {
            void actions.model.archive(profileId);
          }
        }}
        onLogout={() => void logout()}
      />
      <ArchiveManager
        open={archivesOpen}
        loading={archivedModels.isPending || archivedConversations.isPending}
        busy={busy}
        error={archivesOpen ? actions.error : null}
        models={models}
        archivedModels={archivedModels.data ?? []}
        archivedConversations={archivedConversations.data ?? []}
        onClose={() => navigate(returnPath)}
        onRestoreModel={(profileId) => void restoreModel(profileId)}
        onRestoreConversation={(id, profileId) => void restoreConversation(id, profileId)}
      />
      {actions.error && !modelSettingsOpen && !archivesOpen && (
        <div className="error-toast" role="alert"><CircleAlert /><span>{actions.error}</span><button className="icon-button" type="button" onClick={actions.clearError} aria-label="关闭错误"><X /></button></div>
      )}
    </main>
  );
}
