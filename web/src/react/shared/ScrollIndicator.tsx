import {
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

interface ScrollIndicatorProps {
  scrollerRef: RefObject<HTMLElement | null>;
  primary?: boolean;
}

interface ThumbGeometry {
  height: number;
  offset: number;
  maximumScroll: number;
}

export function ScrollIndicator({ scrollerRef, primary = false }: ScrollIndicatorProps) {
  const railRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ startY: number; startScroll: number } | null>(null);
  const [geometry, setGeometry] = useState<ThumbGeometry>({ height: 0, offset: 0, maximumScroll: 0 });
  const [dragging, setDragging] = useState(false);

  const update = useCallback(() => {
    const scroller = scrollerRef.current;
    const rail = railRef.current;
    if (!scroller || !rail) return;
    const viewport = scroller.clientHeight;
    const content = scroller.scrollHeight;
    const maximumScroll = Math.max(0, content - viewport);
    rail.style.top = `${scroller.offsetTop}px`;
    rail.style.height = `${Math.max(0, viewport)}px`;
    const railHeight = Math.max(0, viewport);
    const proportional = content > 0 ? (viewport / content) * railHeight : railHeight;
    const minimum = primary ? 220 : 96;
    const maximum = primary ? 320 : 220;
    const height = maximumScroll === 0
      ? railHeight
      : Math.min(railHeight, Math.max(minimum, Math.min(maximum, proportional)));
    const maximumTravel = Math.max(0, railHeight - height);
    const offset = maximumScroll === 0 ? 0 : (scroller.scrollTop / maximumScroll) * maximumTravel;
    setGeometry({ height, offset, maximumScroll });
  }, [primary, scrollerRef]);

  useEffect(() => {
    const scroller = scrollerRef.current;
    if (!scroller) return undefined;
    update();
    const resizeObserver = new ResizeObserver(update);
    resizeObserver.observe(scroller);
    if (scroller.firstElementChild) resizeObserver.observe(scroller.firstElementChild);
    const mutationObserver = new MutationObserver(update);
    mutationObserver.observe(scroller, { childList: true, subtree: true, characterData: true });
    scroller.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", update, { passive: true });
    return () => {
      resizeObserver.disconnect();
      mutationObserver.disconnect();
      scroller.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
    };
  }, [scrollerRef, update]);

  const moveBy = (nextScrollTop: number) => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    scroller.scrollTop = Math.max(0, Math.min(geometry.maximumScroll, nextScrollTop));
  };

  const handlePointerDown = (event: ReactPointerEvent<HTMLDivElement>) => {
    const scroller = scrollerRef.current;
    if (!scroller || geometry.maximumScroll === 0) return;
    dragRef.current = { startY: event.clientY, startScroll: scroller.scrollTop };
    setDragging(true);
    event.currentTarget.setPointerCapture(event.pointerId);
  };

  const handlePointerMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    const scroller = scrollerRef.current;
    const rail = railRef.current;
    const drag = dragRef.current;
    if (!scroller || !rail || !drag) return;
    const maximumTravel = Math.max(1, rail.clientHeight - geometry.height);
    moveBy(drag.startScroll + ((event.clientY - drag.startY) / maximumTravel) * geometry.maximumScroll);
  };

  const finishDrag = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!dragRef.current) return;
    dragRef.current = null;
    setDragging(false);
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
  };

  const handleKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    const increments: Partial<Record<string, number>> = {
      ArrowUp: -40,
      ArrowDown: 40,
      PageUp: -scroller.clientHeight,
      PageDown: scroller.clientHeight,
      Home: -geometry.maximumScroll,
      End: geometry.maximumScroll,
    };
    const increment = increments[event.key];
    if (increment === undefined) return;
    event.preventDefault();
    moveBy(scroller.scrollTop + increment);
  };

  return (
    <div
      ref={railRef}
      className={`scroll-position-rail${geometry.maximumScroll === 0 ? " is-hidden" : ""}${dragging ? " is-dragging" : ""}`}
      aria-hidden={geometry.maximumScroll === 0}
    >
      <div
        className="scroll-position-thumb"
        role="scrollbar"
        tabIndex={geometry.maximumScroll === 0 ? -1 : 0}
        aria-controls={scrollerRef.current?.id}
        aria-orientation="vertical"
        aria-valuemin={0}
        aria-valuemax={Math.round(geometry.maximumScroll)}
        aria-valuenow={Math.round(scrollerRef.current?.scrollTop ?? 0)}
        style={{ height: geometry.height, transform: `translateY(${geometry.offset}px)` }}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={finishDrag}
        onPointerCancel={finishDrag}
        onKeyDown={handleKeyDown}
      />
    </div>
  );
}
