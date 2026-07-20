import assert from "node:assert/strict";
import test from "node:test";

import {
  ResearchWorkspaceClient,
  ResearchWorkspaceRequestError,
} from "../src/research-workspace-client.ts";

test("protected creates send the supplied idempotency key", async (context) => {
  const originalFetch = globalThis.fetch;
  context.after(() => {
    globalThis.fetch = originalFetch;
  });
  let capturedRequest;
  globalThis.fetch = async (url, options) => {
    capturedRequest = { url, options };
    return new Response(JSON.stringify({ conversation_id: "conversation-1", turns: [] }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };

  const client = new ResearchWorkspaceClient("https://workspace.example");
  await client.createResearchConversation("profile-1", "request-key-0001");

  assert.equal(capturedRequest.url, "https://workspace.example/api/conversations");
  assert.equal(capturedRequest.options.method, "POST");
  assert.equal(capturedRequest.options.headers.get("Idempotency-Key"), "request-key-0001");
});

test("network failures use the public network_unavailable error", async (context) => {
  const originalFetch = globalThis.fetch;
  context.after(() => {
    globalThis.fetch = originalFetch;
  });
  globalThis.fetch = async () => {
    throw new TypeError("offline");
  };

  const client = new ResearchWorkspaceClient();
  await assert.rejects(
    client.listModelProfiles(),
    (error) => error instanceof ResearchWorkspaceRequestError
      && error.status === 0
      && error.code === "network_unavailable"
      && error.retryable,
  );
});

test("trace audit forwards the frozen stage cursor and limit query", async (context) => {
  const originalFetch = globalThis.fetch;
  context.after(() => {
    globalThis.fetch = originalFetch;
  });
  let capturedUrl;
  globalThis.fetch = async (url) => {
    capturedUrl = url;
    return new Response(JSON.stringify({ next_cursor: null, entries: [] }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  };

  const client = new ResearchWorkspaceClient("https://workspace.example");
  await client.loadResearchTraceAudit("conversation-1", "turn-1", {
    stage: "search",
    cursor: 40,
    limit: 20,
  });

  assert.equal(
    capturedUrl,
    "https://workspace.example/api/conversations/conversation-1/turns/turn-1/trace/audit?stage=search&cursor=40&limit=20",
  );
});
