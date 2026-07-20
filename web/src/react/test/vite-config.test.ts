import { describe, expect, it } from "vitest";
import { selectApiProxyTarget } from "../../vite-api-proxy";

describe("Vite API proxy configuration", () => {
  it("prefers an exported shell value over values loaded from env files", () => {
    expect(selectApiProxyTarget("http://127.0.0.1:9000", "http://127.0.0.1:8081"))
      .toBe("http://127.0.0.1:9000");
  });

  it("uses an env-file value and otherwise falls back to the local host", () => {
    expect(selectApiProxyTarget(undefined, "http://127.0.0.1:8081"))
      .toBe("http://127.0.0.1:8081");
    expect(selectApiProxyTarget(undefined, undefined)).toBe("http://127.0.0.1:8080");
  });
});
