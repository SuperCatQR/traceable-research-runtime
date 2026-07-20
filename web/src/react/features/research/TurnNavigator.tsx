import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import type { ResearchTurn } from "../../../research-workspace-client";
import { prefersReducedMotion } from "../../shared/use-document-visibility";

interface TurnNavigatorProps {
  turns: ResearchTurn[];
  scrollerRef: RefObject<HTMLElement | null>;
  onUserNavigate?(): void;
}

export function TurnNavigator({ turns, scrollerRef, onUserNavigate }: TurnNavigatorProps) {
  const [expanded, setExpanded] = useState(false);
  const [activeIndex, setActiveIndex] = useState(Math.max(0, turns.length - 1));
  const headingRef = useRef<HTMLButtonElement>(null);
  const conversationKey = useMemo(() => turns.map((turn) => turn.turn_id).join("|"), [turns]);

  useEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller || turns.length < 2) return undefined;
    const anchors = turns
      .map((turn) => scroller.querySelector<HTMLElement>(`[data-conversation-turn="${CSS.escape(turn.turn_id)}"]`))
      .filter((anchor): anchor is HTMLElement => Boolean(anchor));
    const updateActive = () => {
      const scrollerRect = scroller.getBoundingClientRect();
      const marker = scroller.scrollTop + Math.min(120, scroller.clientHeight * 0.15);
      let next = 0;
      let visibleQuestion = -1;
      anchors.forEach((anchor, index) => {
        const anchorTop = anchor.getBoundingClientRect().top
          - scrollerRect.top
          + scroller.scrollTop;
        if (anchorTop <= marker) next = index;
        const questionRect = (anchor.querySelector<HTMLElement>(".question-block") ?? anchor).getBoundingClientRect();
        if (questionRect.top < scrollerRect.bottom - 60 && questionRect.bottom > scrollerRect.top + 60) {
          visibleQuestion = index;
        }
      });
      if (visibleQuestion >= 0) next = visibleQuestion;
      setActiveIndex(next);
    };
    scroller.addEventListener("scroll", updateActive, { passive: true });
    updateActive();
    return () => scroller.removeEventListener("scroll", updateActive);
  }, [conversationKey, scrollerRef, turns]);

  if (turns.length < 2) return null;

  const jumpTo = (index: number) => {
    const scroller = scrollerRef.current;
    const target = turns[index] && scroller?.querySelector<HTMLElement>(
      `[data-conversation-turn="${CSS.escape(turns[index].turn_id)}"]`,
    );
    if (!scroller || !target) return;
    onUserNavigate?.();
    scroller.scrollTo({
      top: Math.max(
        0,
        target.getBoundingClientRect().top
          - scroller.getBoundingClientRect().top
          + scroller.scrollTop
          - 28,
      ),
      behavior: prefersReducedMotion() ? "auto" : "smooth",
    });
    setExpanded(false);
  };

  return (
    <nav
      className={`turn-navigator${expanded ? " is-expanded" : ""}`}
      aria-label="对话回合快速跳转"
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          setExpanded(false);
          headingRef.current?.focus();
        }
      }}
    >
      <div className="turn-navigator-panel">
        <button
          ref={headingRef}
          type="button"
          className="turn-navigator-heading"
          aria-expanded={expanded}
          onClick={() => setExpanded((value) => !value)}
          title="展开回合索引"
        >
          <span>回合索引</span>
          <strong>{String(activeIndex + 1).padStart(2, "0")}</strong>
        </button>
        <div className="turn-navigator-list">
          {turns.map((turn, index) => (
            <button
              key={turn.turn_id}
              type="button"
              className={index === activeIndex ? "is-active" : ""}
              aria-current={index === activeIndex ? "true" : undefined}
              title={turn.user_question}
              onClick={() => jumpTo(index)}
            >
              <span className="turn-jump-label">{turn.user_question}</span>
              <i className="turn-jump-tick" aria-hidden="true" />
            </button>
          ))}
        </div>
      </div>
    </nav>
  );
}
