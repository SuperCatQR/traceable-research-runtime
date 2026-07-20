import {
  Archive,
  Check,
  CircleAlert,
  CircleCheck,
  KeyRound,
  LoaderCircle,
  LogOut,
  Plus,
  ShieldCheck,
  X,
} from "lucide-react";
import { useEffect, useState } from "react";
import type { ModelProfile, SaveModelProfileInput, UserAccount } from "../../../research-workspace-client";

interface ModelSettingsProps {
  account: UserAccount;
  models: ModelProfile[];
  open: boolean;
  busy: boolean;
  error: string | null;
  onClose(): void;
  onClearError(): void;
  onSave(profileId: string | null, input: SaveModelProfileInput): Promise<boolean>;
  onVerify(profileId: string): void;
  onSetDefault(profileId: string): void;
  onArchive(profileId: string): void;
  onLogout(): void;
}

export function ModelSettings({
  account,
  models,
  open,
  busy,
  error,
  onClose,
  onClearError,
  onSave,
  onVerify,
  onSetDefault,
  onArchive,
  onLogout,
}: ModelSettingsProps) {
  const [editingId, setEditingId] = useState<string | null>(models[0]?.profile_id ?? null);
  const profile = models.find((candidate) => candidate.profile_id === editingId) ?? null;

  useEffect(() => {
    if (editingId && !models.some((candidate) => candidate.profile_id === editingId)) {
      setEditingId(models[0]?.profile_id ?? null);
    }
  }, [editingId, models]);

  useEffect(() => {
    if (!open) return undefined;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose, open]);

  if (!open) return null;
  return (
    <div className="modal-scrim" role="presentation">
      <section className="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="model-settings-title">
        <header className="settings-header">
          <div><span>Workspace settings</span><h2 id="model-settings-title">模型配置</h2></div>
          <button className="icon-button" type="button" onClick={onClose} aria-label="关闭模型配置" title="关闭"><X /></button>
        </header>
        <div className="settings-layout">
          <aside className="model-profile-list">
            <div className="settings-list-heading">
              <span>可用配置</span>
              <button className="icon-button" type="button" onClick={() => { setEditingId(null); onClearError(); }} aria-label="添加模型配置" title="添加"><Plus /></button>
            </div>
            {models.length ? models.map((model) => (
              <button
                key={model.profile_id}
                type="button"
                className={`model-profile-row${profile?.profile_id === model.profile_id ? " is-selected" : ""}`}
                onClick={() => { setEditingId(model.profile_id); onClearError(); }}
              >
                <span><strong>{model.display_name}</strong><code>{model.model_id}</code></span>
                <span className="profile-state">{model.is_default ? "默认" : ""}{model.verified_at ? <CircleCheck aria-label="已验证" /> : null}</span>
              </button>
            )) : <div className="settings-empty"><p>尚无模型配置</p></div>}
          </aside>
          <ModelProfileEditor
            key={profile?.profile_id ?? "new"}
            account={account}
            profile={profile}
            hasOtherModels={models.length > 0}
            busy={busy}
            error={error}
            onSave={(input) => onSave(profile?.profile_id ?? null, input)}
            onVerify={() => profile && onVerify(profile.profile_id)}
            onSetDefault={() => profile && onSetDefault(profile.profile_id)}
            onArchive={() => profile && onArchive(profile.profile_id)}
            onLogout={onLogout}
          />
        </div>
      </section>
    </div>
  );
}

function ModelProfileEditor({
  account,
  profile,
  hasOtherModels,
  busy,
  error,
  onSave,
  onVerify,
  onSetDefault,
  onArchive,
  onLogout,
}: {
  account: UserAccount;
  profile: ModelProfile | null;
  hasOtherModels: boolean;
  busy: boolean;
  error: string | null;
  onSave(input: SaveModelProfileInput): Promise<boolean>;
  onVerify(): void;
  onSetDefault(): void;
  onArchive(): void;
  onLogout(): void;
}) {
  const [displayName, setDisplayName] = useState(profile?.display_name ?? "");
  const [apiBaseUrl, setApiBaseUrl] = useState(profile?.api_base_url ?? "");
  const [modelId, setModelId] = useState(profile?.model_id ?? "");
  const [apiKey, setApiKey] = useState("");
  const [makeDefault, setMakeDefault] = useState(false);
  const existing = Boolean(profile);
  return (
    <div className="model-profile-editor">
      <div className="profile-editor-heading">
        <div><span>{profile ? `Revision ${profile.revision}` : "New profile"}</span><h3>{profile?.display_name ?? "添加模型配置"}</h3></div>
        {profile && <span className={`verification-status${profile.verified_at ? " is-verified" : ""}`}>{profile.verified_at ? <ShieldCheck /> : <CircleAlert />}{profile.verified_at ? "已验证" : "未验证"}</span>}
      </div>
      <form
        className="stacked-form profile-form"
        onSubmit={(event) => {
          event.preventDefault();
          if (busy) return;
          void onSave({
            display_name: displayName.trim(),
            api_base_url: apiBaseUrl.trim(),
            model_id: modelId.trim(),
            ...(apiKey ? { api_key: apiKey } : {}),
            ...(!existing ? { make_default: makeDefault } : {}),
          }).then((saved) => {
            if (saved) setApiKey("");
          });
        }}
      >
        <label>配置名称<input value={displayName} onChange={(event) => setDisplayName(event.target.value)} required maxLength={80} placeholder="主要模型" /></label>
        <label>API 地址<input value={apiBaseUrl} onChange={(event) => setApiBaseUrl(event.target.value)} type="url" required maxLength={2048} placeholder="https://api.example.com/v1/" /></label>
        <label>模型 ID<input value={modelId} onChange={(event) => setModelId(event.target.value)} required maxLength={200} placeholder="model-name" /></label>
        <label>API Key<input value={apiKey} onChange={(event) => setApiKey(event.target.value)} type="password" required={!existing} maxLength={4096} autoComplete="new-password" placeholder={existing ? "留空以保留当前密钥" : "输入 API Key"} /></label>
        {!existing && hasOtherModels && <label className="checkbox-field"><input checked={makeDefault} onChange={(event) => setMakeDefault(event.target.checked)} type="checkbox" />设为默认配置</label>}
        <p className="credential-note"><KeyRound />密钥加密保存，保存后不可查看</p>
        {error && <div className="inline-error" role="alert"><CircleAlert /><span>{error}</span></div>}
        <div className="profile-form-actions">
          <button className="primary-command" type="submit" disabled={busy}>{busy ? <LoaderCircle className="spin" /> : <Check />}{existing ? "保存更改" : "保存配置"}</button>
          {profile && <button className="secondary-command" type="button" onClick={onVerify} disabled={busy}><ShieldCheck />验证连接</button>}
        </div>
      </form>
      {profile && <div className="profile-management-actions">
        {!profile.is_default && <button className="text-command" type="button" onClick={onSetDefault} disabled={busy}>设为默认</button>}
        <button className="text-command danger" type="button" onClick={onArchive} disabled={busy}><Archive />归档配置</button>
      </div>}
      <button className="logout-command" type="button" onClick={onLogout} disabled={busy}><LogOut />退出 {account.display_name}</button>
    </div>
  );
}
