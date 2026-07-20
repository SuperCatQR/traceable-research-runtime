import "@testing-library/jest-dom/vitest";

class TestResizeObserver implements ResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

Object.defineProperty(globalThis, "ResizeObserver", { value: TestResizeObserver, configurable: true });
Object.defineProperty(window, "matchMedia", {
  configurable: true,
  value: (query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => undefined,
    removeListener: () => undefined,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
    dispatchEvent: () => false,
  }),
});
Object.defineProperty(HTMLElement.prototype, "scrollTo", {
  configurable: true,
  value(options: ScrollToOptions) {
    if (typeof options.top === "number") this.scrollTop = options.top;
  },
});
if (!globalThis.CSS) Object.defineProperty(globalThis, "CSS", { value: {} });
if (!globalThis.CSS.escape) globalThis.CSS.escape = (value: string) => value.replace(/[^a-zA-Z0-9_-]/g, "\\$&");
