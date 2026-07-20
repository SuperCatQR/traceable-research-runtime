import {
  Archive,
  Check,
  Menu,
  Moon,
  PanelRightOpen,
  Pencil,
  Settings2,
  Sun,
  X,
} from "lucide-react";
import { useEffect, useState } from "react";
import type { ModelProfile, ResearchConversationDetail } from "../../../research-workspace-client";
import type { Theme } from "../../shared/use-theme";

interface WorkspaceHeaderProps {
  conversation?: ResearchConversationDetail;
  models: ModelProfile[];
  theme: Theme;
  busy: boolean;
  onOpenSidebar(): void;
  onRename(title: string): Promise<boolean>;
  onChangeModel(profileId: string): void;
  onToggleTheme(): void;
  onToggleInspector(): void;
  onOpenSettings(): void;
  onArchive(): void;
}

export function WorkspaceHeader({
  conversation,
  models,
  theme,
  busy,
  onOpenSidebar,
  onRename,
  onChangeModel,
  onToggleTheme,
  onToggleInspector,
  onOpenSettings,
  onArchive,
}: WorkspaceHeaderProps) {
  const [editing, setEditing] = useState(false);
  const [title, setTitle] = useState(conversation?.title ?? "");

  useEffect(() => {
    setTitle(conversation?.title ?? "");
    setEditing(false);
  }, [conversation?.conversation_id, conversation?.title]);

  return (
    <header className="workspace-header">
      <button className="icon-button mobile-only" type="button" onClick={onOpenSidebar} aria-label="打开对话列表" title="对话列表">
        <Menu aria-hidden="true" />
      </button>
      {conversation ? editing ? (
        <form
          className="title-editor"
          onSubmit={(event) => {
            event.preventDefault();
            const next = title.trim();
            if (!next || busy) return;
            void onRename(next).then((saved) => {
              if (saved) setEditing(false);
            });
          }}
        >
          <input value={title} onChange={(event) => setTitle(event.target.value)} maxLength={200} required aria-label="对话标题" autoFocus />
          <button className="icon-button" type="submit" aria-label="保存标题" title="保存" disabled={busy}><Check /></button>
          <button className="icon-button" type="button" onClick={() => setEditing(false)} aria-label="取消编辑" title="取消"><X /></button>
        </form>
      ) : (
        <div className="conversation-heading document-title">
          <span className="title-kicker">Research workspace</span>
          <h1>{conversation.title}</h1>
          <button className="icon-button subtle" type="button" onClick={() => setEditing(true)} aria-label="重命名对话" title="重命名">
            <Pencil aria-hidden="true" />
          </button>
        </div>
      ) : (
        <div className="conversation-heading document-title">
          <span className="title-kicker">Research workspace</span><h1>研究工作区</h1>
        </div>
      )}
      <div className="workspace-header-actions header-actions">
        {conversation && models.length > 0 && (
          <label className="model-profile-selector model-button">
            <span className="sr-only">当前模型配置</span>
            <select value={conversation.model_profile_id} onChange={(event) => onChangeModel(event.target.value)} disabled={busy}>
              {models.map((profile) => (
                <option key={profile.profile_id} value={profile.profile_id}>{profile.display_name} · {profile.model_id}</option>
              ))}
            </select>
          </label>
        )}
        <button className="icon-button theme-toggle" type="button" onClick={onToggleTheme} aria-label={`切换到${theme === "light" ? "深色" : "浅色"}主题`} title="切换主题">
          {theme === "light" ? <Moon aria-hidden="true" /> : <Sun aria-hidden="true" />}
        </button>
        {conversation?.turns.length ? (
          <button className="icon-button" type="button" onClick={onToggleInspector} aria-label="打开研究概览" title="研究概览">
            <PanelRightOpen aria-hidden="true" />
          </button>
        ) : null}
        <button className="icon-button" type="button" onClick={onOpenSettings} aria-label="模型配置" title="模型配置">
          <Settings2 aria-hidden="true" />
        </button>
        {conversation && (
          <button className="icon-button danger-hover" type="button" onClick={onArchive} aria-label="归档对话" title="归档">
            <Archive aria-hidden="true" />
          </button>
        )}
      </div>
    </header>
  );
}
