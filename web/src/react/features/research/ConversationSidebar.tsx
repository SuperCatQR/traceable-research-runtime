import { Archive, Search, Settings2, SquarePen, X } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import type {
  ResearchConversationSummary,
  UserAccount,
} from "../../../research-workspace-client";
import { ScrollIndicator } from "../../shared/ScrollIndicator";
import { formatActivityDate } from "../../shared/format";
import { statusLabels } from "./status-labels";

interface ConversationSidebarProps {
  account: UserAccount;
  conversations: ResearchConversationSummary[];
  activeConversationId?: string;
  open: boolean;
  busy: boolean;
  onClose(): void;
  onCreate(): void;
  onSelect(conversationId: string): void;
  onOpenArchives(): void;
  onOpenSettings(): void;
}
export function ConversationSidebar({
  account,
  conversations,
  activeConversationId,
  open,
  busy,
  onClose,
  onCreate,
  onSelect,
  onOpenArchives,
  onOpenSettings,
}: ConversationSidebarProps) {
  const [search, setSearch] = useState("");
  const listRef = useRef<HTMLElement>(null);
  const filtered = useMemo(() => {
    const query = search.trim().toLocaleLowerCase();
    if (!query) return conversations;
    return conversations.filter((conversation) => (
      `${conversation.title} ${conversation.model_profile_name}`.toLocaleLowerCase().includes(query)
    ));
  }, [conversations, search]);

  return (
    <aside className={`conversation-sidebar sidebar scroll-indicator-host${open ? " is-open" : ""}`} aria-label="研究对话">
      <header className="sidebar-header brand-row">
        <div className="product-identity brand-lockup">
          <span className="brand-mark" aria-hidden="true" />
          <div><strong>Traceable</strong><span>Research</span></div>
        </div>
        <button className="icon-button mobile-only" type="button" onClick={onClose} aria-label="关闭对话列表" title="关闭">
          <X aria-hidden="true" />
        </button>
      </header>
      <button className="new-conversation-command new-research" type="button" onClick={onCreate} disabled={busy}>
        <SquarePen aria-hidden="true" /><span>新研究</span>
      </button>
      <label className="conversation-search sidebar-search">
        <Search aria-hidden="true" />
        <span className="sr-only">搜索研究对话</span>
        <input
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          type="search"
          placeholder="搜索对话"
          autoComplete="off"
        />
      </label>
      <nav ref={listRef} id="conversation-list" className="conversation-list" aria-label="对话列表">
        {filtered.length ? filtered.map((conversation) => {
          const active = activeConversationId === conversation.conversation_id;
          const status = conversation.latest_turn_status;
          const stateClass = status === "running" || status === "ready"
            ? " is-running"
            : status === "failed" ? " has-error" : "";
          return (
            <button
              key={conversation.conversation_id}
              type="button"
              className={`conversation-list-item conversation-item${active ? " is-active" : ""}${stateClass}`}
              aria-current={active ? "page" : undefined}
              onClick={() => onSelect(conversation.conversation_id)}
            >
              <span className="conversation-list-title conversation-title">{conversation.title}</span>
              <span className="conversation-list-meta conversation-meta">
                <span>{conversation.turn_count} 轮{status ? ` · ${statusLabels[status]}` : ""}</span>
                <time dateTime={new Date(conversation.updated_at * 1000).toISOString()}>
                  {formatActivityDate(conversation.updated_at)}
                </time>
              </span>
            </button>
          );
        }) : <div className="sidebar-empty"><p>{search ? "没有匹配的研究" : "尚无研究对话"}</p></div>}
      </nav>
      <ScrollIndicator scrollerRef={listRef} />
      <footer className="sidebar-footer">
        <button type="button" className="archive-manager-command archive-link" onClick={onOpenArchives}>
          <Archive aria-hidden="true" /><span>归档</span>
        </button>
        <button type="button" className="account-command account-card" onClick={onOpenSettings}>
          <span className="account-monogram">{account.display_name.slice(0, 1).toUpperCase()}</span>
          <span><strong>{account.display_name}</strong><small>{account.email}</small></span>
          <Settings2 aria-hidden="true" />
        </button>
      </footer>
    </aside>
  );
}
