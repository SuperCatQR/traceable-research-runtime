import { CircleAlert, LoaderCircle, RotateCcw, X } from "lucide-react";
import { useEffect, useState } from "react";
import type {
  ArchivedModelProfile,
  ArchivedResearchConversation,
  ModelProfile,
} from "../../../research-workspace-client";

interface ArchiveManagerProps {
  open: boolean;
  loading: boolean;
  busy: boolean;
  error: string | null;
  models: ModelProfile[];
  archivedModels: ArchivedModelProfile[];
  archivedConversations: ArchivedResearchConversation[];
  onClose(): void;
  onRestoreModel(profileId: string): void;
  onRestoreConversation(conversationId: string, modelProfileId?: string): void;
}
export function ArchiveManager({
  open,
  loading,
  busy,
  error,
  models,
  archivedModels,
  archivedConversations,
  onClose,
  onRestoreModel,
  onRestoreConversation,
}: ArchiveManagerProps) {
  const [replacements, setReplacements] = useState<Record<string, string>>({});

  useEffect(() => {
    if (!open) return undefined;
    const closeOnEscape = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose, open]);

  if (!open) return null;
  const defaultModel = models.find((profile) => profile.is_default)?.profile_id ?? models[0]?.profile_id;
  return (
    <div className="modal-scrim" role="presentation">
      <section className="settings-dialog archive-dialog" role="dialog" aria-modal="true" aria-labelledby="archive-manager-title">
        <header className="settings-header">
          <div><span>Workspace archive</span><h2 id="archive-manager-title">归档与恢复</h2></div>
          <button className="icon-button" type="button" onClick={onClose} aria-label="关闭归档" title="关闭"><X /></button>
        </header>
        {loading ? <div className="inspector-status"><LoaderCircle className="spin" /><span>正在读取归档</span></div> : (
          <div className="archive-sections">
            <section className="archive-section" aria-labelledby="archived-conversations-title">
              <h3 id="archived-conversations-title">对话 <span>{archivedConversations.length}</span></h3>
              <ul>{archivedConversations.length ? archivedConversations.map((conversation) => {
                const replacement = replacements[conversation.conversation_id] ?? defaultModel;
                return <li className="archive-row" key={conversation.conversation_id}>
                  <span className="archive-row-identity"><strong>{conversation.title}</strong><small>{conversation.turn_count} 轮 · {conversation.model_profile_name}</small></span>
                  {!conversation.model_profile_available && <label className="archive-replacement"><span>恢复到</span><select value={replacement ?? ""} onChange={(event) => setReplacements((current) => ({ ...current, [conversation.conversation_id]: event.target.value }))}>{models.map((profile) => <option key={profile.profile_id} value={profile.profile_id}>{profile.display_name}</option>)}</select></label>}
                  <button className="secondary-command" type="button" onClick={() => onRestoreConversation(conversation.conversation_id, conversation.model_profile_available ? undefined : replacement)} disabled={busy || (!conversation.model_profile_available && !replacement)}><RotateCcw />恢复</button>
                </li>;
              }) : <li className="archive-empty">没有已归档对话</li>}</ul>
            </section>
            <section className="archive-section" aria-labelledby="archived-profiles-title">
              <h3 id="archived-profiles-title">模型配置 <span>{archivedModels.length}</span></h3>
              <ul>{archivedModels.length ? archivedModels.map((profile) => <li className="archive-row" key={profile.profile_id}>
                <span className="archive-row-identity"><strong>{profile.display_name}</strong><small>{profile.model_id}</small></span>
                <button className="secondary-command" type="button" onClick={() => onRestoreModel(profile.profile_id)} disabled={busy}><RotateCcw />恢复</button>
              </li>) : <li className="archive-empty">没有已归档模型配置</li>}</ul>
            </section>
          </div>
        )}
        {error && <div className="inline-error" role="alert"><CircleAlert /><span>{error}</span></div>}
      </section>
    </div>
  );
}
