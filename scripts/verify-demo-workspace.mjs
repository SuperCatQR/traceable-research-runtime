import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer as createHttpServer } from "node:http";
import { createServer as createNetServer } from "node:net";
import { mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { randomBytes } from "node:crypto";

const projectRoot = resolve(import.meta.dirname, "..");
const demoHostBinary = resolve(
  projectRoot,
  "demo-host",
  "target",
  "debug",
  process.platform === "win32" ? "traceable-search-demo-host.exe" : "traceable-search-demo-host",
);
const temporaryDataDirectory = await mkdtemp(join(tmpdir(), "traceable-demo-http-"));
const credentialEncryptionKey = randomBytes(32).toString("base64");
const testApiKey = `smoke-key-${randomBytes(12).toString("hex")}`;
const hostPort = await reserveAvailablePort();
const hostBaseUrl = `http://127.0.0.1:${hostPort}`;

let demoHostProcess;
let demoHostOutput = "";
const modelRequestKinds = [];
const searchQueries = [];
let holdSearchResponses = false;
const blockedSearchResponseReleasers = [];

const modelServer = createHttpServer(async (request, response) => {
  try {
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const requestBody = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    const systemPrompt = requestBody.messages?.[0]?.content ?? "";
    const userPrompt = requestBody.messages?.[1]?.content ?? "";
    const modelContent = buildModelResponse(systemPrompt, userPrompt);
    response.writeHead(200, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ choices: [{ message: { content: JSON.stringify(modelContent) } }] }));
  } catch {
    response.writeHead(400, { "Content-Type": "application/json" });
    response.end(JSON.stringify({ error: "invalid test request" }));
  }
});

const searchServer = createHttpServer(async (request, response) => {
  const requestUrl = new URL(request.url ?? "/", "http://127.0.0.1");
  if (request.method !== "GET" || requestUrl.pathname !== "/search") {
    response.writeHead(404).end();
    return;
  }
  searchQueries.push(requestUrl.searchParams.get("q") ?? "");
  if (holdSearchResponses) {
    await new Promise((release) => blockedSearchResponseReleasers.push(release));
  }
  response.writeHead(200, { "Content-Type": "application/json" });
  // The result passes the search parser but is rejected by the public-page SSRF
  // boundary. That makes the automatic execution path fully local and deterministic.
  response.end(JSON.stringify({
    results: [{
      title: "Local fixture result",
      url: "http://127.0.0.1:1/blocked-by-ssrf",
      content: "Fixture result used to exercise automatic research execution.",
    }],
  }));
});

const modelApiBaseUrl = await listenOnLoopback(modelServer, "/v1/");
const searchApiBaseUrl = await listenOnLoopback(searchServer, "/");

try {
  await startDemoHost();

  const workspaceEntryResponse = await fetch(`${hostBaseUrl}/`);
  assert.equal(workspaceEntryResponse.status, 200);
  assert.equal(workspaceEntryResponse.headers.get("cache-control"), "no-store");
  const workspaceEntryHtml = await workspaceEntryResponse.text();
  const workspaceFallbackResponse = await fetch(`${hostBaseUrl}/conversations/layout-regression`);
  assert.equal(workspaceFallbackResponse.status, 404);
  assert.equal(workspaceFallbackResponse.headers.get("cache-control"), "no-store");
  const hashedAssetPath = workspaceEntryHtml.match(/(?:src|href)="(\/assets\/[^\"]+)"/)?.[1];
  assert.ok(hashedAssetPath, "workspace entry must reference a hashed asset");
  const hashedAssetResponse = await fetch(`${hostBaseUrl}${hashedAssetPath}`);
  assert.equal(hashedAssetResponse.status, 200);
  assert.equal(
    hashedAssetResponse.headers.get("cache-control"),
    "public, max-age=31536000, immutable",
  );

  const accountACookie = await registerAccount("researcher-a@example.com", "Researcher A");
  const currentAccount = await requestJson("/api/auth/me", { cookie: accountACookie });
  assert.equal(currentAccount.body.email, "researcher-a@example.com");

  const profileResponse = await requestJson("/api/model-profiles", {
    method: "POST",
    cookie: accountACookie,
    body: {
      display_name: "Smoke model",
      api_base_url: modelApiBaseUrl,
      api_key: testApiKey,
      model_id: "smoke-model",
      make_default: true,
    },
  });
  const modelProfile = profileResponse.body;
  assert.equal(modelProfile.has_api_key, true);
  assert.equal(Object.hasOwn(modelProfile, "api_key"), false);
  assert.equal(Object.hasOwn(modelProfile, "api_key_ciphertext"), false);
  assert.equal(profileResponse.rawBody.includes(testApiKey), false);

  const conversationResponse = await requestJson("/api/conversations", {
    method: "POST",
    cookie: accountACookie,
    body: { model_profile_id: modelProfile.profile_id },
  });
  const conversationId = conversationResponse.body.conversation_id;

  const accountBCookie = await registerAccount("researcher-b@example.com", "Researcher B");
  const accountBConversations = await requestJson("/api/conversations", { cookie: accountBCookie });
  assert.deepEqual(accountBConversations.body, []);
  await requestJson(`/api/conversations/${conversationId}`, {
    cookie: accountBCookie,
    expectedStatus: 404,
  });
  await requestJson(`/api/model-profiles/${modelProfile.profile_id}`, {
    method: "PATCH",
    cookie: accountBCookie,
    body: { display_name: "Not owned" },
    expectedStatus: 404,
  });

  const researchQuestion = "Which evidence should guide a traceable research runtime?";
  const turnResponse = await requestJson(`/api/conversations/${conversationId}/turns`, {
    method: "POST",
    cookie: accountACookie,
    body: { question: researchQuestion },
  });
  const turnId = turnResponse.body.turn_id;
  const traceSummaryResponse = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/trace/summary`,
    { cookie: accountACookie },
  );
  const traceAuditResponse = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/trace/audit`,
    { cookie: accountACookie },
  );
  assert.equal(turnResponse.body.status, "clarifying");
  assert.equal(turnResponse.body.dialogue.status, "awaiting_message");
  assert.equal(
    turnResponse.body.dialogue.messages.at(-1).text,
    "I understand you want evidence for a traceable research runtime. Tell me which source qualities matter most.",
  );
  assert.equal(traceSummaryResponse.body.run_id, null);
  assert.equal(
    traceSummaryResponse.body.clarification_rationale_audit_status,
    "required_and_validated",
  );
  assert.equal(traceSummaryResponse.body.research_rationale_audit_status, null);
  assert.equal(
    traceSummaryResponse.body.understanding.rationale,
    "Source priorities materially affect retrieval and evidence selection.",
  );
  assert.equal(Object.hasOwn(traceSummaryResponse.body, "clarification_events"), false);
  assert.equal(Object.hasOwn(traceSummaryResponse.body, "research_events"), false);
  assert.equal(JSON.stringify(traceAuditResponse.body).includes("brief_draft"), false);
  assert.equal(JSON.stringify(traceAuditResponse.body).includes("original_question"), false);
  assert(traceAuditResponse.body.entries.some((entry) => (
    entry.stage === "dialogue"
      && entry.rationale === "Source priorities materially affect retrieval and evidence selection."
  )));

  await requestJson(`/api/conversations/${conversationId}/turns/${turnId}/execute`, {
    method: "POST",
    cookie: accountACookie,
    expectedStatus: 405,
  });
  await requestJson(`/api/conversations/${conversationId}/turns/${turnId}/confirm`, {
    method: "POST",
    cookie: accountACookie,
    expectedStatus: 405,
  });

  await requestJson(`/api/conversations/${conversationId}/turns/${turnId}/trace/summary`, {
    cookie: accountBCookie,
    expectedStatus: 404,
  });
  await requestJson(`/api/conversations/${conversationId}/turns/${turnId}/trace/audit`, {
    cookie: accountBCookie,
    expectedStatus: 404,
  });

  await requestJson(`/api/model-profiles/${modelProfile.profile_id}`, {
    method: "PATCH",
    cookie: accountACookie,
    body: { display_name: "Locked profile" },
    expectedStatus: 409,
  });
  await requestJson(`/api/conversations/${conversationId}`, {
    method: "PATCH",
    cookie: accountACookie,
    body: { title: "Locked title" },
    expectedStatus: 409,
  });

  const scheduledAutomaticResearchResponse = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/messages`,
    {
      method: "POST",
      cookie: accountACookie,
      body: {
        revision: turnResponse.body.dialogue.revision,
        message: "Prioritize primary sources and official technical documentation.",
      },
    },
  );
  assert(["ready", "running"].includes(scheduledAutomaticResearchResponse.body.status));
  assert.equal(
    scheduledAutomaticResearchResponse.body.dialogue.messages.at(-1).text,
    "I understand the source preference and am starting research now.",
  );
  const automaticResearchTurn = await waitForTurnStatus(
    conversationId,
    turnId,
    accountACookie,
    "failed",
  );
  assert.equal(automaticResearchTurn.dialogue.status, "failed");
  assert.equal(automaticResearchTurn.answer, null);
  assert.equal(typeof automaticResearchTurn.run_id, "string");
  assert(modelRequestKinds.includes("dialogue"));
  assert(modelRequestKinds.includes("planning"));
  assert(searchQueries.length >= 3, "automatic research must invoke search without a second user command");

  const automaticTraceSummary = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/trace/summary`,
    { cookie: accountACookie },
  );
  const automaticTraceAudit = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/trace/audit`,
    { cookie: accountACookie },
  );
  assert.equal(automaticTraceSummary.body.run_id, automaticResearchTurn.run_id);
  assert(automaticTraceSummary.body.failure, "automatic research failure must be traceable");
  assert(automaticTraceSummary.body.rounds.length >= 1);
  assert(automaticTraceSummary.body.skipped_source_count >= 1);
  assert(automaticTraceAudit.body.entries.some((entry) => (
    entry.stage === "dialogue"
      && entry.rationale === "The supplied preference resolves the remaining source-selection uncertainty."
  )));
  assert(automaticTraceAudit.body.entries.some((entry) => entry.stage === "failure"));
  const setupTraceAudit = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/trace/audit?stage=setup`,
    { cookie: accountACookie },
  );
  assert(setupTraceAudit.body.entries.length > 0);
  assert(setupTraceAudit.body.entries.every((entry) => entry.stage === "setup"));

  const firstDialogueAuditPage = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/trace/audit?stage=dialogue&limit=1`,
    { cookie: accountACookie },
  );
  assert.equal(firstDialogueAuditPage.body.entries.length, 1);
  assert.equal(firstDialogueAuditPage.body.next_cursor, 1);
  const secondDialogueAuditPage = await requestJson(
    `/api/conversations/${conversationId}/turns/${turnId}/trace/audit?stage=dialogue&cursor=${firstDialogueAuditPage.body.next_cursor}&limit=1`,
    { cookie: accountACookie },
  );
  assert.equal(secondDialogueAuditPage.body.entries.length, 1);

  const traceDirectory = join(temporaryDataDirectory, "traces");
  await rm(traceDirectory, { recursive: true, force: true });
  await writeFile(traceDirectory, "forced trace setup failure");
  const preparationFailureConversation = await requestJson("/api/conversations", {
    method: "POST",
    cookie: accountACookie,
    body: { model_profile_id: modelProfile.profile_id },
  });
  const preparationFailureConversationId = preparationFailureConversation.body.conversation_id;
  const preparationFailureTurn = await requestJson(
    `/api/conversations/${preparationFailureConversationId}/turns`,
    {
      method: "POST",
      cookie: accountACookie,
      body: { question: "Force a prepared research setup failure." },
    },
  );
  const preparationFailureTurnId = preparationFailureTurn.body.turn_id;
  const scheduledPreparationFailureResponse = await requestJson(
    `/api/conversations/${preparationFailureConversationId}/turns/${preparationFailureTurnId}/messages`,
    {
      method: "POST",
      cookie: accountACookie,
      body: {
        revision: preparationFailureTurn.body.dialogue.revision,
        message: "Use official sources.",
      },
    },
  );
  assert(["ready", "running"].includes(scheduledPreparationFailureResponse.body.status));
  const preparationFailureResponse = await waitForTurnStatus(
    preparationFailureConversationId,
    preparationFailureTurnId,
    accountACookie,
    "failed",
  );
  assert.equal(preparationFailureResponse.dialogue.status, "failed");
  assert.equal(preparationFailureResponse.run_id, null);
  const preparationFailureSummary = await requestJson(
    `/api/conversations/${preparationFailureConversationId}/turns/${preparationFailureTurnId}/trace/summary`,
    { cookie: accountACookie },
  );
  assert.equal(preparationFailureSummary.body.failure.stage, "initialization");
  const preparationFailureAudit = await requestJson(
    `/api/conversations/${preparationFailureConversationId}/turns/${preparationFailureTurnId}/trace/audit`,
    { cookie: accountACookie },
  );
  assert(preparationFailureAudit.body.entries.some((entry) => (
    entry.stage === "failure"
      && ["研究准备失败", "研究运行初始化失败"].includes(entry.label)
  )));
  await rm(traceDirectory, { force: true });
  const followUpAfterPreparationFailure = await requestJson(
    `/api/conversations/${preparationFailureConversationId}/turns`,
    {
      method: "POST",
      cookie: accountACookie,
      body: { question: "Can this conversation start a new natural-language turn?" },
    },
  );
  assert.equal(followUpAfterPreparationFailure.body.status, "clarifying");
  assert.equal(followUpAfterPreparationFailure.body.dialogue.status, "awaiting_message");

  const recoveryConversationResponse = await requestJson("/api/conversations", {
    method: "POST",
    cookie: accountACookie,
    body: { model_profile_id: modelProfile.profile_id },
  });
  const recoveryConversationId = recoveryConversationResponse.body.conversation_id;
  const recoveryTurnResponse = await requestJson(
    `/api/conversations/${recoveryConversationId}/turns`,
    {
      method: "POST",
      cookie: accountACookie,
      body: { question: "Recover this automatic research run after a host restart." },
    },
  );
  const recoveryTurnId = recoveryTurnResponse.body.turn_id;
  assert.equal(recoveryTurnResponse.body.dialogue.status, "awaiting_message");

  const queriesBeforeInterruptedRun = searchQueries.length;
  holdSearchResponses = true;
  const scheduledAutomaticTurn = await Promise.race([
    requestJson(
      `/api/conversations/${recoveryConversationId}/turns/${recoveryTurnId}/messages`,
      {
        method: "POST",
        cookie: accountACookie,
        body: {
          revision: recoveryTurnResponse.body.dialogue.revision,
          message: "Use official documentation first.",
        },
      },
    ),
    new Promise((_, reject) => setTimeout(
      () => reject(new Error("automatic research scheduling blocked the dialogue response")),
      2_000,
    )),
  ]);
  assert(
    ["ready", "running"].includes(scheduledAutomaticTurn.body.status),
    "model-approved research must return a pending turn before execution completes",
  );
  await waitForCondition(
    () => searchQueries.length > queriesBeforeInterruptedRun,
    "interrupted automatic run did not reach the search stage",
  );
  await stopDemoHost();
  releaseBlockedSearchResponses();
  holdSearchResponses = false;

  await startDemoHost();
  const recoveredTurn = await waitForTurnStatus(
    recoveryConversationId,
    recoveryTurnId,
    accountACookie,
    "failed",
  );
  assert.equal(recoveredTurn.dialogue.status, "failed");
  assert.equal(
    recoveredTurn.dialogue.messages.at(-1).text,
    "I understand the source preference and am starting research now.",
  );
  assert(
    searchQueries.length > queriesBeforeInterruptedRun + 1,
    "startup recovery must resume research without another browser command",
  );

  await stopDemoHost();
  await startDemoHost();

  const restoredAccount = await requestJson("/api/auth/me", { cookie: accountACookie });
  assert.equal(restoredAccount.body.email, "researcher-a@example.com");
  const restoredConversation = await requestJson(`/api/conversations/${conversationId}`, {
    cookie: accountACookie,
  });
  assert.equal(restoredConversation.body.title, researchQuestion);
  assert.equal(restoredConversation.body.turns.length, 1);
  assert.equal(restoredConversation.body.turns[0].status, "failed");
  assert.equal(restoredConversation.body.turns[0].run_id, automaticResearchTurn.run_id);
  assert.equal(restoredConversation.body.turns[0].dialogue.status, "failed");
  assert.equal(
    restoredConversation.body.turns[0].dialogue.messages.at(-1).text,
    "I understand the source preference and am starting research now.",
  );

  const restoredProfiles = await requestJson("/api/model-profiles", { cookie: accountACookie });
  assert.equal(restoredProfiles.body.length, 1);
  assert.equal(restoredProfiles.rawBody.includes(testApiKey), false);

  await requestJson("/api/auth/logout", {
    method: "POST",
    cookie: accountACookie,
    expectedStatus: 204,
  });
  await requestJson("/api/auth/me", { cookie: accountACookie, expectedStatus: 401 });

  await stopDemoHost();
  await assertFilesDoNotContain(temporaryDataDirectory, testApiKey);

  console.log("Demo workspace HTTP verification passed:");
  console.log("- HTML entries are not stored while hashed static assets are cached as immutable");
  console.log("- account cookies survive host restart and logout revokes them");
  console.log("- conversations and automatic dialogue-driven research turns restore after restart");
  console.log("- users cannot access another account's conversations, model profiles, or Trace routes");
  console.log("- active turns lock profile and conversation mutation");
  console.log("- Trace summary excludes raw events and audit excludes the hidden structured brief");
  console.log("- model start_research decisions return promptly and run automatically without a second browser command");
  console.log("- startup recovery resumes interrupted automatic research without making conversation reads stateful");
  console.log("- API credentials are absent from responses and persisted plaintext files");
} catch (error) {
  if (demoHostOutput) console.error(demoHostOutput.trim());
  throw error;
} finally {
  releaseBlockedSearchResponses();
  await stopDemoHost();
  await closeHttpServer(modelServer);
  await closeHttpServer(searchServer);
  await rm(temporaryDataDirectory, { recursive: true, force: true });
}

function buildModelResponse(systemPrompt, userPrompt) {
  if (systemPrompt === "Return JSON only.") {
    modelRequestKinds.push("verification");
    return { ok: true };
  }
  if (systemPrompt.includes('"decision":"continue_dialogue"')) {
    modelRequestKinds.push("dialogue");
    return buildDialogueResponse(userPrompt);
  }
  if (systemPrompt.includes('"queries"')) {
    modelRequestKinds.push("planning");
    return buildSearchPlan(userPrompt);
  }
  throw new Error("unsupported model prompt in workspace verifier");
}

function buildDialogueResponse(userPrompt) {
  const input = JSON.parse(userPrompt);
  const dialogue = Array.isArray(input.dialogue) ? input.dialogue : [];
  const hasUserFollowUp = dialogue.filter((message) => message.role === "user").length > 1;
  return {
    decision: hasUserFollowUp ? "start_research" : "continue_dialogue",
    rationale: hasUserFollowUp
      ? "The supplied preference resolves the remaining source-selection uncertainty."
      : "Source priorities materially affect retrieval and evidence selection.",
    assistant_message: hasUserFollowUp
      ? "I understand the source preference and am starting research now."
      : "I understand you want evidence for a traceable research runtime. Tell me which source qualities matter most.",
    brief_draft: {
      schema_version: 1,
      original_question: input.original_question,
      research_question: input.original_question,
      desired_output: null,
      scope: { time_range: null, geography: null, include: [], exclude: [] },
      source_constraints: [],
      accepted_assumptions: [],
    },
  };
}

function buildSearchPlan(userPrompt) {
  const input = JSON.parse(userPrompt);
  const existingQueryCount = Array.isArray(input.previous_queries)
    ? input.previous_queries.length
    : 0;
  return {
    queries: [0, 1, 2].map((offset) => ({
      query: `traceability evidence ${existingQueryCount + offset + 1}`,
      gap: "Fixture search coverage.",
    })),
  };
}

async function registerAccount(email, displayName) {
  const response = await requestJson("/api/auth/register", {
    method: "POST",
    body: { email, display_name: displayName, password: "correct horse battery staple" },
  });
  const setCookie = response.headers.get("set-cookie");
  assert(setCookie, "register response must set a login cookie");
  return setCookie.split(";", 1)[0];
}

async function requestJson(
  requestPath,
  { method = "GET", cookie, body, expectedStatus = 200 } = {},
) {
  const response = await fetch(`${hostBaseUrl}${requestPath}`, {
    method,
    headers: {
      ...(cookie ? { Cookie: cookie } : {}),
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const rawBody = await response.text();
  assert.equal(
    response.status,
    expectedStatus,
    `${method} ${requestPath} returned ${response.status}: ${rawBody}`,
  );
  return {
    headers: response.headers,
    rawBody,
    body: rawBody ? JSON.parse(rawBody) : null,
  };
}

async function startDemoHost() {
  demoHostOutput = "";
  demoHostProcess = spawn(demoHostBinary, [], {
    cwd: projectRoot,
    env: {
      ...process.env,
      SEARCH_BASE_URL: searchApiBaseUrl,
      CRAWL4AI_BASE_URL: "http://127.0.0.1:9/",
      CRAWL4AI_TOKEN: "",
      STRONG_MODEL_BASE_URL: modelApiBaseUrl,
      STRONG_MODEL_API_KEY: "legacy-smoke-key",
      STRONG_MODEL_ID: "smoke-model",
      TRACEABLE_SEARCH_DATA_DIR: temporaryDataDirectory,
      DEMO_STATIC_DIR: resolve(projectRoot, "demo", "dist"),
      DEMO_CATALOG_PATH: resolve(temporaryDataDirectory, "demo-catalog.sqlite"),
      DEMO_CREDENTIAL_ENCRYPTION_KEY: credentialEncryptionKey,
      DEMO_ALLOW_PRIVATE_MODEL_ENDPOINTS: "true",
      DEMO_SECURE_COOKIES: "false",
      DEMO_BIND: `127.0.0.1:${hostPort}`,
      RUST_LOG: "traceable_search_demo_host=warn",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  demoHostProcess.stdout.on("data", (chunk) => { demoHostOutput += chunk.toString(); });
  demoHostProcess.stderr.on("data", (chunk) => { demoHostOutput += chunk.toString(); });
  await waitForHealth();
}

async function stopDemoHost() {
  if (!demoHostProcess || demoHostProcess.exitCode !== null) return;
  const exited = new Promise((resolveExit) => demoHostProcess.once("exit", resolveExit));
  demoHostProcess.kill();
  await Promise.race([
    exited,
    new Promise((resolveTimeout) => setTimeout(resolveTimeout, 3_000)),
  ]);
  if (demoHostProcess.exitCode === null) demoHostProcess.kill("SIGKILL");
  demoHostProcess = undefined;
}

async function waitForHealth() {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (demoHostProcess.exitCode !== null) {
      throw new Error(`demo host exited before becoming healthy\n${demoHostOutput}`);
    }
    try {
      const response = await fetch(`${hostBaseUrl}/api/health`);
      if (response.ok) return;
    } catch {
      // Startup races are expected until the listener is ready.
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 100));
  }
  throw new Error(`demo host did not become healthy\n${demoHostOutput}`);
}

function releaseBlockedSearchResponses() {
  while (blockedSearchResponseReleasers.length > 0) {
    blockedSearchResponseReleasers.pop()();
  }
}

async function waitForCondition(predicate, message) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  }
  throw new Error(message);
}

async function waitForTurnStatus(conversationId, turnId, cookie, expectedStatus) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const conversation = await requestJson(`/api/conversations/${conversationId}`, { cookie });
    const turn = conversation.body.turns.find((candidate) => candidate.turn_id === turnId);
    if (turn?.status === expectedStatus) return turn;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 50));
  }
  throw new Error(`research turn ${turnId} did not reach ${expectedStatus}`);
}

async function listenOnLoopback(server, pathSuffix) {
  await new Promise((resolveListening) => server.listen(0, "127.0.0.1", resolveListening));
  const address = server.address();
  assert(address && typeof address !== "string");
  return `http://127.0.0.1:${address.port}${pathSuffix}`;
}

async function closeHttpServer(server) {
  await new Promise((resolveClose) => server.close(resolveClose));
}

async function reserveAvailablePort() {
  const server = createNetServer();
  await new Promise((resolveListening) => server.listen(0, "127.0.0.1", resolveListening));
  const address = server.address();
  assert(address && typeof address !== "string");
  await new Promise((resolveClose) => server.close(resolveClose));
  return address.port;
}

async function assertFilesDoNotContain(directory, forbiddenText) {
  for (const entry of await readdir(directory)) {
    const path = join(directory, entry);
    const metadata = await stat(path);
    if (metadata.isDirectory()) {
      await assertFilesDoNotContain(path, forbiddenText);
    } else {
      const content = await readFile(path);
      assert.equal(content.includes(Buffer.from(forbiddenText)), false, `${path} contains plaintext API key`);
    }
  }
}
