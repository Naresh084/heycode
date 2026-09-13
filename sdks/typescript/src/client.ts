export const APP_SERVER_PROTOCOL_VERSION = 1 as const;
const MAX_WIRE_BYTES = 4 * 1024 * 1024;

export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

export type AppServerErrorCode =
  | "invalid_request"
  | "method_not_found"
  | "conflict"
  | "cancelled"
  | "unsupported"
  | "unavailable"
  | "closed"
  | "internal";

export class HeycodeError extends Error {
  readonly code: AppServerErrorCode;

  constructor(code: AppServerErrorCode) {
    super(errorMessage(code));
    this.name = "HeycodeError";
    this.code = code;
  }
}

export interface HeycodeTransport {
  exchange(
    request: string,
    onNotification: (notification: string) => Promise<void>,
    signal?: AbortSignal,
  ): Promise<string>;
}

export type AppTurnReason = "stop" | "limit" | "cancelled" | "error";

export interface TokenUsage {
  prompt_tokens: number;
  completion_tokens: number;
}

export interface RuntimeContextUsage {
  resolved_model?: string;
  tokens: number;
  context_window: number;
}

export interface AttachmentMetadata {
  content_id: string;
  media_type: string;
  byte_len: number;
  display_name?: string;
  dimensions?: { width: number; height: number };
  audio?: {
    duration_ms: number;
    sample_rate_hz: number;
    channels: number;
    bits_per_sample: number;
  };
  source?: JsonValue;
}

export interface DocumentInputRoute {
  kind: "native" | "extracted";
  source: AttachmentMetadata;
  selected: AttachmentMetadata;
}

export type AppServerEvent =
  | {
      type: "user_input";
      text: string;
      attachments: AttachmentMetadata[];
      document_routes: DocumentInputRoute[];
    }
  | { type: "turn_started"; turn_id: string }
  | { type: "assistant_delta"; text: string }
  | { type: "assistant_audio"; attachments: AttachmentMetadata[] }
  | { type: "reasoning_delta"; text: string }
  | { type: "tool_started"; call_id: string; name: string; arguments: JsonValue }
  | {
      type: "tool_finished";
      call_id: string;
      name: string;
      result: JsonValue;
      ok: boolean;
      untrusted_content?: JsonValue;
    }
  | { type: "usage"; usage: TokenUsage; context?: RuntimeContextUsage }
  | { type: "plan_changed"; active: boolean }
  | { type: "notice"; code: string; message: string }
  | {
      type: "authorization_prompt_requested";
      prompt_id: number;
      prompt: string;
      reference: string;
      kind: string;
      masked: boolean;
    }
  | { type: "authorization_prompt_resolved"; prompt_id: number; answered: boolean }
  | { type: "permission_requested"; request_id: string; action: string; detail: string }
  | {
      type: "question_requested";
      request_id: string;
      header?: string;
      prompt: string;
      choices: string[];
      choice_descriptions?: Array<string | null>;
    }
  | {
      type: "turn_finished";
      turn_id: string;
      reason: AppTurnReason;
      usage?: TokenUsage;
    };

export interface AppServerNotification {
  jsonrpc: "2.0";
  method: "session/event" | "control/event";
  params: {
    sessionId?: string;
    sequence: number;
    event: AppServerEvent;
  };
}

export interface AppSessionInfo {
  sessionId: string;
  runtimeId: string;
  cwd: string;
  configuration: AppRuntimeConfiguration;
  configurationCapabilities: AppRuntimeConfigurationCapabilities;
}

export interface ToolSpec {
  name: string;
  description: string;
  parameters: JsonValue;
}

export interface AppRuntimeConfiguration {
  systemPrompt?: string;
  tools?: ToolSpec[];
  model?: string;
  reasoningEffort?: string;
}

export interface AppRuntimeConfigurationCapabilities {
  systemPrompt: AppCapabilityEvidence;
  tools: AppCapabilityEvidence;
  model: AppCapabilityEvidence;
  reasoningEffort: AppCapabilityEvidence;
}

export interface AppRuntimeModelConfiguration {
  model: string;
  defaultReasoningEffort: string | null;
  reasoningEfforts: string[];
}

export interface AppTurnResult {
  turnId: string | null;
  reason: AppTurnReason;
}

export type AppPermissionDecision = "allow_once" | "allow_session" | "deny";

export interface AppServerCapabilities {
  turns: boolean;
  attachments: boolean;
  cancel: boolean;
  authorization: boolean;
  models: boolean;
  runtimes: boolean;
  workspace: boolean;
  mcp: boolean;
  plugins: boolean;
  settings: boolean;
}

export interface AppInitializeResult {
  protocolVersion: number;
  server: { name: string; version: string };
  capabilities: AppServerCapabilities;
}

export type AppCredentialValidation =
  | { state: "unknown" }
  | { state: "valid"; checked_at_ms: number }
  | { state: "invalid"; checked_at_ms: number; reason: string }
  | { state: "stale"; checked_at_ms: number };

export interface AppCredentialStatus {
  reference: string;
  kind: string;
  inspected: boolean;
  configured: boolean | null;
  source: string | null;
  provider: string | null;
  writable: boolean | null;
  validation: AppCredentialValidation;
}

export interface AppAuthorizationFlow {
  id: string;
  label: string;
  method: string;
  interactive: boolean;
  provider: string | null;
  credential: AppCredentialStatus;
}

export interface AppAuthorizationReceipt {
  provider: string;
  flow: string;
  committedBy: string;
  credential: AppCredentialStatus;
}

export interface AppLogoutResult {
  deletedFrom: string | null;
}

export interface AppRouteSelection {
  runtime: string;
  provider: string;
  model: string;
  effort: string | null;
}

export interface AppProviderRow {
  id: string;
  displayName: string;
  defaultModel: string;
  credentialReference: string | null;
  protocols: string[];
}

export interface AppProviderCatalog {
  current: AppRouteSelection;
  providers: AppProviderRow[];
}

export type AppRuntimeWorkspace = "composed" | "selectable";

export interface AppRuntimeCapabilities {
  models: AppCapabilityEvidence;
  resume: AppCapabilityEvidence;
  fork: AppCapabilityEvidence;
  steer: AppCapabilityEvidence;
  followUp: AppCapabilityEvidence;
  permissions: AppCapabilityEvidence;
  questions: AppCapabilityEvidence;
  compaction: AppCapabilityEvidence;
}

export interface AppRuntimeRow {
  id: string;
  displayName: string;
  kind: "native" | "delegated";
  workspace: AppRuntimeWorkspace;
  capabilities: AppRuntimeCapabilities;
  configuration: AppRuntimeConfigurationCapabilities;
}

export interface AppRuntimeCatalog {
  current: AppRouteSelection;
  runtimes: AppRuntimeRow[];
}

export interface AppWorkspaceSelection {
  cwd: string;
  runtime: string;
  selected: boolean;
}

export type AppCapabilityEvidence = "supported" | "unsupported" | "unknown";
export type AppModelLifecycle = "unknown" | "stable" | "preview" | "deprecated" | "retired";
export type AppCatalogFreshness = "live" | "fresh_cache" | "stale_fallback" | "default_fallback";
export type AppCatalogRefresh = "prefer_cache" | "force";

export interface AppModelCapabilities {
  tools: AppCapabilityEvidence;
  reasoning: AppCapabilityEvidence;
  imageInput: AppCapabilityEvidence;
  documentInput: AppCapabilityEvidence;
  structuredOutput: AppCapabilityEvidence;
  nativeWeb: AppCapabilityEvidence;
  nativeCompaction: AppCapabilityEvidence;
  promptCache: AppCapabilityEvidence;
}

export interface AppModelRow {
  id: string;
  displayName: string;
  aliases: string[];
  contextWindow: number | null;
  maxOutputTokens: number | null;
  lifecycle: AppModelLifecycle;
  selectable: boolean;
  replacementIds: string[];
  capabilities: AppModelCapabilities;
}

export interface AppModelCatalog {
  provider: string;
  currentModel: string;
  defaultModel: string;
  revision: number | null;
  fetchedAtMs: number | null;
  freshness: AppCatalogFreshness;
  warning: { code: string; message: string } | null;
  models: AppModelRow[];
}

export interface AppPluginInventory {
  plugins: Array<{ id: string; version: string; source: string; scope: string }>;
  contributions: Array<{ plugin: string; kind: string; name: string }>;
}

export interface AppSettingsSnapshot {
  namespace: string;
  exposed: boolean;
  writable: boolean;
  applies: string;
  revision: number;
  schema: JsonValue | null;
  defaults: JsonValue | null;
  base: JsonValue | null;
  user: JsonValue | null;
  project: JsonValue | null;
  resolved: JsonValue | null;
}

export class HeycodeClient {
  readonly #transport: HeycodeTransport;
  #nextId = 1;
  #session: AppSessionInfo | null = null;

  constructor(transport: HeycodeTransport) {
    this.#transport = transport;
  }

  async initialize(signal?: AbortSignal): Promise<AppInitializeResult> {
    const value = await this.#call("initialize", {}, parseInitialize, undefined, signal);
    if (value.protocolVersion !== APP_SERVER_PROTOCOL_VERSION) {
      throw new HeycodeError("invalid_request");
    }
    return value;
  }

  async start(signal?: AbortSignal): Promise<AppSessionInfo> {
    return this.startWithConfiguration({}, signal);
  }

  async startWithConfiguration(
    configuration: AppRuntimeConfiguration,
    signal?: AbortSignal,
  ): Promise<AppSessionInfo> {
    await this.initialize(signal);
    const session = await this.#call(
      "session/open",
      { configuration },
      parseSession,
      undefined,
      signal,
    );
    this.#session = session;
    return session;
  }

  async resume(expectedSessionId: string, signal?: AbortSignal): Promise<AppSessionInfo> {
    return this.resumeWithConfiguration(expectedSessionId, {}, signal);
  }

  async resumeWithConfiguration(
    expectedSessionId: string,
    configuration: AppRuntimeConfiguration,
    signal?: AbortSignal,
  ): Promise<AppSessionInfo> {
    await this.initialize(signal);
    const session = await this.#call(
      "session/open",
      { configuration },
      parseSession,
      undefined,
      signal,
    );
    if (session.sessionId !== expectedSessionId) {
      throw new HeycodeError("invalid_request");
    }
    this.#session = session;
    return session;
  }

  async open(signal?: AbortSignal): Promise<AppSessionInfo> {
    return this.start(signal);
  }

  async configure(
    configuration: AppRuntimeConfiguration,
    signal?: AbortSignal,
  ): Promise<AppSessionInfo> {
    const session = this.#session;
    if (session === null) throw new HeycodeError("invalid_request");
    const configured = await this.#call(
      "session/configure",
      { sessionId: session.sessionId, configuration },
      parseSession,
      undefined,
      signal,
    );
    if (configured.sessionId !== session.sessionId) {
      throw new HeycodeError("invalid_request");
    }
    this.#session = configured;
    return configured;
  }

  async runtimeModels(signal?: AbortSignal): Promise<AppRuntimeModelConfiguration[]> {
    return this.#call(
      "runtime/models",
      {},
      value => parseArray(value).map(parseRuntimeModelConfiguration),
      undefined,
      signal,
    );
  }

  async turn(
    text: string,
    attachments: AttachmentMetadata[],
    onEvent: (event: AppServerNotification) => void | Promise<void>,
    signal?: AbortSignal,
  ): Promise<AppTurnResult> {
    const session = this.#session;
    if (session === null) {
      throw new HeycodeError("invalid_request");
    }
    return this.#call(
      "turn/start",
      { sessionId: session.sessionId, text, attachments },
      parseTurnResult,
      onEvent,
      signal,
    );
  }

  async cancel(signal?: AbortSignal): Promise<void> {
    await this.#call("turn/cancel", {}, parseNull, undefined, signal);
  }

  async respondPermission(
    requestId: string,
    decision: AppPermissionDecision,
    signal?: AbortSignal,
  ): Promise<void> {
    const session = this.#session;
    if (session === null) throw new HeycodeError("invalid_request");
    await this.#call(
      "session/permission/respond",
      { sessionId: session.sessionId, requestId, decision },
      parseNull,
      undefined,
      signal,
    );
  }

  async respondQuestion(requestId: string, answer: string, signal?: AbortSignal): Promise<void> {
    const session = this.#session;
    if (session === null) throw new HeycodeError("invalid_request");
    await this.#call(
      "session/question/respond",
      { sessionId: session.sessionId, requestId, answer },
      parseNull,
      undefined,
      signal,
    );
  }

  async cancelQuestion(requestId: string, signal?: AbortSignal): Promise<void> {
    const session = this.#session;
    if (session === null) throw new HeycodeError("invalid_request");
    await this.#call(
      "session/question/respond",
      { sessionId: session.sessionId, requestId, cancelled: true },
      parseNull,
      undefined,
      signal,
    );
  }

  async close(signal?: AbortSignal): Promise<void> {
    await this.#call("session/close", {}, parseNull, undefined, signal);
    this.#session = null;
  }

  async authorization(signal?: AbortSignal): Promise<AppAuthorizationFlow[]> {
    return this.#call("authorization/list", {}, value => parseArray(value).map(parseAuthorizationFlow), undefined, signal);
  }

  async authorize(
    flowId: string,
    onEvent: (event: AppServerNotification) => void | Promise<void>,
    signal?: AbortSignal,
  ): Promise<AppAuthorizationReceipt> {
    return this.#call(
      "authorization/start",
      { flowId },
      parseAuthorizationReceipt,
      onEvent,
      signal,
    );
  }

  async answerAuthorization(promptId: number, secret: string, signal?: AbortSignal): Promise<void> {
    await this.#call("authorization/answer", { promptId, secret }, parseNull, undefined, signal);
  }

  async cancelAuthorization(promptId: number, signal?: AbortSignal): Promise<void> {
    await this.#call("authorization/cancel", { promptId }, parseNull, undefined, signal);
  }

  async logout(provider: string | null = null, signal?: AbortSignal): Promise<AppLogoutResult> {
    return this.#call(
      "authorization/logout",
      { provider },
      parseLogoutResult,
      undefined,
      signal,
    );
  }

  async providers(signal?: AbortSignal): Promise<AppProviderCatalog> {
    return this.#call("providers/list", {}, parseProviderCatalog, undefined, signal);
  }

  async selectProvider(provider: string, signal?: AbortSignal): Promise<AppRouteSelection> {
    return this.#call(
      "providers/select",
      { provider },
      parseRoute,
      undefined,
      signal,
    );
  }

  async models(
    provider: string | null = null,
    refresh: AppCatalogRefresh = "prefer_cache",
    signal?: AbortSignal,
  ): Promise<AppModelCatalog> {
    return this.#call(
      "models/list",
      { provider, refresh },
      parseModelCatalog,
      undefined,
      signal,
    );
  }

  async selectModel(model: string, signal?: AbortSignal): Promise<AppRouteSelection> {
    return this.#call(
      "models/select",
      { model },
      parseRoute,
      undefined,
      signal,
    );
  }

  async runtimes(signal?: AbortSignal): Promise<AppRuntimeCatalog> {
    return this.#call("runtimes/list", {}, parseRuntimeCatalog, undefined, signal);
  }

  async selectRuntime(runtime: string, signal?: AbortSignal): Promise<AppRouteSelection> {
    return this.#call(
      "runtime/select",
      { runtime },
      parseRoute,
      undefined,
      signal,
    );
  }

  async selectWorkspace(cwd: string, signal?: AbortSignal): Promise<AppWorkspaceSelection> {
    return this.#call(
      "workspace/select",
      { cwd },
      parseWorkspaceSelection,
      undefined,
      signal,
    );
  }

  async mcp(signal?: AbortSignal): Promise<JsonValue> {
    return this.#call("mcp/list", {}, parseJsonValue, undefined, signal);
  }

  async plugins(signal?: AbortSignal): Promise<AppPluginInventory> {
    return this.#call("plugins/list", {}, parsePluginInventory, undefined, signal);
  }

  async settings(signal?: AbortSignal): Promise<AppSettingsSnapshot[]> {
    return this.#call("settings/list", {}, value => parseArray(value).map(parseSettings), undefined, signal);
  }

  async setting(namespace: string, signal?: AbortSignal): Promise<AppSettingsSnapshot> {
    return this.#call(
      "settings/get",
      { namespace },
      parseSettings,
      undefined,
      signal,
    );
  }

  async replaceSetting(
    namespace: string,
    user: { [key: string]: JsonValue },
    expectedRevision: number,
    signal?: AbortSignal,
  ): Promise<AppSettingsSnapshot> {
    return this.#call(
      "settings/replace",
      { namespace, user, expectedRevision },
      parseSettings,
      undefined,
      signal,
    );
  }

  async #call<R>(
    method: string,
    params: Record<string, unknown>,
    parseResult: (value: unknown) => R,
    onEvent?: (event: AppServerNotification) => void | Promise<void>,
    signal?: AbortSignal,
  ): Promise<R> {
    if (!Number.isSafeInteger(this.#nextId) || this.#nextId <= 0) {
      throw new HeycodeError("internal");
    }
    const id = this.#nextId;
    this.#nextId += 1;
    const request = JSON.stringify({ jsonrpc: "2.0", id, method, params });
    assertWireBound(request);
    let firstSequence = true;
    let lastSequence = 0;
    let response: string;
    try {
      response = await this.#transport.exchange(
        request,
        async raw => {
          assertWireBound(raw);
          const notification = decodeNotification(raw);
          if (notification.method === "session/event") {
            if (this.#session === null || notification.params.sessionId !== this.#session.sessionId) {
              throw new HeycodeError("invalid_request");
            }
          } else if (notification.params.sessionId !== undefined) {
            throw new HeycodeError("invalid_request");
          }
          if (!firstSequence && notification.params.sequence !== lastSequence + 1) {
            throw new HeycodeError("invalid_request");
          }
          firstSequence = false;
          lastSequence = notification.params.sequence;
          await onEvent?.(notification);
        },
        signal,
      );
    } catch (error: unknown) {
      if (error instanceof HeycodeError) throw error;
      if (signal?.aborted === true) throw new HeycodeError("cancelled");
      throw new HeycodeError("unavailable");
    }
    assertWireBound(response);
    const envelope = parseJson(response);
    assertRecord(envelope);
    assertKeys(envelope, ["jsonrpc", "id", "result", "error"]);
    if (envelope.jsonrpc !== "2.0" || envelope.id !== id) {
      throw new HeycodeError("invalid_request");
    }
    if (envelope.error !== undefined) {
      assertRecord(envelope.error);
      throw new HeycodeError(mapRpcCode(expectNumber(envelope.error.code)));
    }
    if (!("result" in envelope)) {
      throw new HeycodeError("invalid_request");
    }
    return parseResult(envelope.result);
  }
}

function parseInitialize(value: unknown): AppInitializeResult {
  assertRecord(value);
  assertKeys(value, ["protocolVersion", "server", "capabilities"]);
  assertRecord(value.server);
  assertKeys(value.server, ["name", "version"]);
  assertRecord(value.capabilities);
  assertKeys(value.capabilities, [
    "turns", "attachments", "cancel", "authorization", "models", "runtimes", "workspace",
    "mcp", "plugins", "settings",
  ]);
  return {
    protocolVersion: expectNumber(value.protocolVersion),
    server: { name: expectString(value.server.name), version: expectString(value.server.version) },
    capabilities: {
      turns: expectBoolean(value.capabilities.turns),
      attachments: expectBoolean(value.capabilities.attachments),
      cancel: expectBoolean(value.capabilities.cancel),
      authorization: expectBoolean(value.capabilities.authorization),
      models: expectBoolean(value.capabilities.models),
      runtimes: optionalBoolean(value.capabilities.runtimes, false),
      workspace: optionalBoolean(value.capabilities.workspace, false),
      mcp: expectBoolean(value.capabilities.mcp),
      plugins: expectBoolean(value.capabilities.plugins),
      settings: expectBoolean(value.capabilities.settings),
    },
  };
}

function parseSession(value: unknown): AppSessionInfo {
  assertRecord(value);
  assertKeys(value, [
    "sessionId", "runtimeId", "cwd", "configuration", "configurationCapabilities",
  ]);
  return {
    sessionId: expectString(value.sessionId),
    runtimeId: expectString(value.runtimeId),
    cwd: expectString(value.cwd),
    configuration: value.configuration === undefined
      ? {}
      : parseRuntimeConfiguration(value.configuration),
    configurationCapabilities: value.configurationCapabilities === undefined
      ? unknownRuntimeConfigurationCapabilities()
      : parseRuntimeConfigurationCapabilities(value.configurationCapabilities),
  };
}

function parseRuntimeConfiguration(value: unknown): AppRuntimeConfiguration {
  assertRecord(value);
  assertKeys(value, ["systemPrompt", "tools", "model", "reasoningEffort"]);
  const configuration: AppRuntimeConfiguration = {};
  if (value.systemPrompt !== undefined) configuration.systemPrompt = expectString(value.systemPrompt);
  if (value.tools !== undefined) configuration.tools = parseArray(value.tools).map(parseToolSpec);
  if (value.model !== undefined) configuration.model = expectString(value.model);
  if (value.reasoningEffort !== undefined) {
    configuration.reasoningEffort = expectString(value.reasoningEffort);
  }
  return configuration;
}

function parseToolSpec(value: unknown): ToolSpec {
  assertRecord(value);
  assertKeys(value, ["name", "description", "parameters"]);
  return {
    name: expectString(value.name),
    description: expectString(value.description),
    parameters: parseJsonValue(value.parameters),
  };
}

function parseRuntimeConfigurationCapabilities(
  value: unknown,
): AppRuntimeConfigurationCapabilities {
  assertRecord(value);
  assertKeys(value, ["systemPrompt", "tools", "model", "reasoningEffort"]);
  return {
    systemPrompt: evidence(value.systemPrompt),
    tools: evidence(value.tools),
    model: evidence(value.model),
    reasoningEffort: evidence(value.reasoningEffort),
  };
}

function unknownRuntimeConfigurationCapabilities(): AppRuntimeConfigurationCapabilities {
  return {
    systemPrompt: "unknown",
    tools: "unknown",
    model: "unknown",
    reasoningEffort: "unknown",
  };
}

function parseRuntimeModelConfiguration(value: unknown): AppRuntimeModelConfiguration {
  assertRecord(value);
  assertKeys(value, ["model", "defaultReasoningEffort", "reasoningEfforts"]);
  return {
    model: expectString(value.model),
    defaultReasoningEffort: nullableString(value.defaultReasoningEffort),
    reasoningEfforts: stringArray(value.reasoningEfforts),
  };
}

function parseTurnResult(value: unknown): AppTurnResult {
  assertRecord(value);
  assertKeys(value, ["turnId", "reason"]);
  const turnId = value.turnId;
  if (turnId !== null && typeof turnId !== "string") throw new HeycodeError("invalid_request");
  return { turnId, reason: parseReason(value.reason) };
}

export function decodeNotification(raw: string): AppServerNotification {
  const value = parseJson(raw);
  assertRecord(value);
  assertKeys(value, ["jsonrpc", "method", "params"]);
  if (value.jsonrpc !== "2.0" || (value.method !== "session/event" && value.method !== "control/event")) {
    throw new HeycodeError("invalid_request");
  }
  assertRecord(value.params);
  assertKeys(value.params, ["sessionId", "sequence", "event"]);
  const sessionId = value.params.sessionId;
  if (sessionId !== undefined && typeof sessionId !== "string") throw new HeycodeError("invalid_request");
  const params: AppServerNotification["params"] = {
    sequence: expectNumber(value.params.sequence),
    event: parseEvent(value.params.event),
  };
  if (sessionId !== undefined) params.sessionId = sessionId;
  return { jsonrpc: "2.0", method: value.method, params };
}

function parseEvent(value: unknown): AppServerEvent {
  assertRecord(value);
  const type = expectString(value.type);
  switch (type) {
    case "user_input":
      return {
        type,
        text: expectString(value.text),
        attachments: parseArray(value.attachments).map(parseAttachment),
        document_routes: parseArray(value.document_routes).map(parseDocumentRoute),
      };
    case "turn_started": return { type, turn_id: expectString(value.turn_id) };
    case "assistant_delta": return { type, text: expectString(value.text) };
    case "assistant_audio": return {
      type,
      attachments: parseArray(value.attachments).map(parseAttachment),
    };
    case "reasoning_delta": return { type, text: expectString(value.text) };
    case "tool_started": return {
      type, call_id: expectString(value.call_id), name: expectString(value.name),
      arguments: parseJsonValue(value.arguments),
    };
    case "tool_finished": {
      const event: Extract<AppServerEvent, { type: "tool_finished" }> = {
        type, call_id: expectString(value.call_id), name: expectString(value.name),
        result: parseJsonValue(value.result), ok: expectBoolean(value.ok),
      };
      if (value.untrusted_content !== undefined) event.untrusted_content = parseJsonValue(value.untrusted_content);
      return event;
    }
    case "usage": {
      const event: Extract<AppServerEvent, { type: "usage" }> = {
        type,
        usage: parseUsage(value.usage),
      };
      if (value.context !== undefined) event.context = parseRuntimeContextUsage(value.context);
      return event;
    }
    case "plan_changed": return { type, active: expectBoolean(value.active) };
    case "notice": return { type, code: expectString(value.code), message: expectString(value.message) };
    case "authorization_prompt_requested": return {
      type, prompt_id: expectNumber(value.prompt_id), prompt: expectString(value.prompt),
      reference: expectString(value.reference), kind: expectString(value.kind),
      masked: expectBoolean(value.masked),
    };
    case "authorization_prompt_resolved": return {
      type, prompt_id: expectNumber(value.prompt_id), answered: expectBoolean(value.answered),
    };
    case "permission_requested": return {
      type,
      request_id: expectString(value.request_id),
      action: expectString(value.action),
      detail: expectString(value.detail),
    };
    case "question_requested": {
      const event: Extract<AppServerEvent, { type: "question_requested" }> = {
        type,
        request_id: expectString(value.request_id),
        prompt: expectString(value.prompt),
        choices: stringArray(value.choices),
      };
      if (value.header !== undefined) event.header = expectString(value.header);
      if (value.choice_descriptions !== undefined) {
        event.choice_descriptions = parseArray(value.choice_descriptions).map(description => {
          if (description === null) return null;
          return expectString(description);
        });
      }
      return event;
    }
    case "turn_finished": {
      const event: Extract<AppServerEvent, { type: "turn_finished" }> = {
        type, turn_id: expectString(value.turn_id), reason: parseReason(value.reason),
      };
      if (value.usage !== undefined) event.usage = parseUsage(value.usage);
      return event;
    }
    default: throw new HeycodeError("invalid_request");
  }
}

function parseAttachment(value: unknown): AttachmentMetadata {
  assertRecord(value);
  assertKeys(value, ["content_id", "media_type", "byte_len", "display_name", "dimensions", "audio", "source"]);
  const result: AttachmentMetadata = {
    content_id: expectString(value.content_id),
    media_type: expectString(value.media_type),
    byte_len: expectNumber(value.byte_len),
  };
  if (value.display_name !== undefined) result.display_name = expectString(value.display_name);
  if (value.dimensions !== undefined) {
    assertRecord(value.dimensions);
    assertKeys(value.dimensions, ["width", "height"]);
    result.dimensions = {
      width: expectNumber(value.dimensions.width),
      height: expectNumber(value.dimensions.height),
    };
  }
  if (value.audio !== undefined) {
    assertRecord(value.audio);
    assertKeys(value.audio, ["duration_ms", "sample_rate_hz", "channels", "bits_per_sample"]);
    const duration_ms = expectNumber(value.audio.duration_ms);
    const sample_rate_hz = expectNumber(value.audio.sample_rate_hz);
    const channels = expectNumber(value.audio.channels);
    const bits_per_sample = expectNumber(value.audio.bits_per_sample);
    if (duration_ms < 1 || duration_ms > 86_400_000
      || sample_rate_hz < 8_000 || sample_rate_hz > 384_000
      || channels < 1 || channels > 8
      || ![8, 16, 24, 32].includes(bits_per_sample)) {
      throw new HeycodeError("invalid_request");
    }
    result.audio = { duration_ms, sample_rate_hz, channels, bits_per_sample };
  }
  if (value.source !== undefined) result.source = parseJsonValue(value.source);
  return result;
}

function parseDocumentRoute(value: unknown): DocumentInputRoute {
  assertRecord(value);
  const kind = value.kind;
  if (kind !== "native" && kind !== "extracted") throw new HeycodeError("invalid_request");
  return { kind, source: parseAttachment(value.source), selected: parseAttachment(value.selected) };
}

function parseUsage(value: unknown): TokenUsage {
  assertRecord(value);
  return {
    prompt_tokens: expectNumber(value.prompt_tokens),
    completion_tokens: expectNumber(value.completion_tokens),
  };
}

function parseRuntimeContextUsage(value: unknown): RuntimeContextUsage {
  assertRecord(value);
  assertKeys(value, ["tokens", "context_window", "resolved_model"]);
  const tokens = expectNumber(value.tokens);
  const context_window = expectNumber(value.context_window);
  if (tokens < 0 || !Number.isSafeInteger(tokens)
    || context_window < 1 || !Number.isSafeInteger(context_window)) {
    throw new HeycodeError("invalid_request");
  }
  return { tokens, context_window, ...(value.resolved_model === undefined ? {} : { resolved_model: expectString(value.resolved_model) }) };
}

function parseReason(value: unknown): AppTurnReason {
  if (value === "stop" || value === "limit" || value === "cancelled" || value === "error") return value;
  throw new HeycodeError("invalid_request");
}

function parseAuthorizationFlow(value: unknown): AppAuthorizationFlow {
  assertRecord(value);
  assertKeys(value, ["id", "label", "method", "interactive", "provider", "credential"]);
  return {
    id: expectString(value.id),
    label: expectString(value.label),
    method: expectString(value.method),
    interactive: expectBoolean(value.interactive),
    provider: nullableString(value.provider),
    credential: parseCredential(value.credential),
  };
}

function parseAuthorizationReceipt(value: unknown): AppAuthorizationReceipt {
  assertRecord(value);
  assertKeys(value, ["provider", "flow", "committedBy", "credential"]);
  return {
    provider: expectString(value.provider),
    flow: expectString(value.flow),
    committedBy: expectString(value.committedBy),
    credential: parseCredential(value.credential),
  };
}

function parseCredential(value: unknown): AppCredentialStatus {
  assertRecord(value);
  assertKeys(value, [
    "reference", "kind", "inspected", "configured", "source", "provider", "writable", "validation",
  ]);
  return {
    reference: expectString(value.reference),
    kind: expectString(value.kind),
    inspected: expectBoolean(value.inspected),
    configured: nullableBoolean(value.configured),
    source: nullableString(value.source),
    provider: nullableString(value.provider),
    writable: nullableBoolean(value.writable),
    validation: parseCredentialValidation(value.validation),
  };
}

function parseCredentialValidation(value: unknown): AppCredentialValidation {
  assertRecord(value);
  const state = expectString(value.state);
  switch (state) {
    case "unknown":
      assertKeys(value, ["state"]);
      return { state };
    case "valid":
      assertKeys(value, ["state", "checked_at_ms"]);
      return { state, checked_at_ms: expectNumber(value.checked_at_ms) };
    case "invalid":
      assertKeys(value, ["state", "checked_at_ms", "reason"]);
      return {
        state,
        checked_at_ms: expectNumber(value.checked_at_ms),
        reason: expectString(value.reason),
      };
    case "stale":
      assertKeys(value, ["state", "checked_at_ms"]);
      return { state, checked_at_ms: expectNumber(value.checked_at_ms) };
    default:
      throw new HeycodeError("invalid_request");
  }
}

function parseLogoutResult(value: unknown): AppLogoutResult {
  assertRecord(value);
  assertKeys(value, ["deletedFrom"]);
  return { deletedFrom: nullableString(value.deletedFrom) };
}

function parseRoute(value: unknown): AppRouteSelection {
  assertRecord(value);
  assertKeys(value, ["runtime", "provider", "model", "effort"]);
  return {
    runtime: expectString(value.runtime),
    provider: expectString(value.provider),
    model: expectString(value.model),
    effort: nullableString(value.effort),
  };
}

function parseProviderCatalog(value: unknown): AppProviderCatalog {
  assertRecord(value);
  assertKeys(value, ["current", "providers"]);
  return {
    current: parseRoute(value.current),
    providers: parseArray(value.providers).map(parseProvider),
  };
}

function parseProvider(value: unknown): AppProviderRow {
  assertRecord(value);
  assertKeys(value, ["id", "displayName", "defaultModel", "credentialReference", "protocols"]);
  return {
    id: expectString(value.id),
    displayName: expectString(value.displayName),
    defaultModel: expectString(value.defaultModel),
    credentialReference: nullableString(value.credentialReference),
    protocols: stringArray(value.protocols),
  };
}

function parseRuntimeCatalog(value: unknown): AppRuntimeCatalog {
  assertRecord(value);
  assertKeys(value, ["current", "runtimes"]);
  return {
    current: parseRoute(value.current),
    runtimes: parseArray(value.runtimes).map(parseRuntime),
  };
}

function parseRuntime(value: unknown): AppRuntimeRow {
  assertRecord(value);
  assertKeys(value, [
    "id", "displayName", "kind", "workspace", "capabilities", "configuration",
  ]);
  if (value.kind !== "native" && value.kind !== "delegated") {
    throw new HeycodeError("invalid_request");
  }
  if (value.workspace !== "composed" && value.workspace !== "selectable") {
    throw new HeycodeError("invalid_request");
  }
  assertRecord(value.capabilities);
  assertKeys(value.capabilities, [
    "models", "resume", "fork", "steer", "followUp", "permissions", "questions", "compaction",
  ]);
  return {
    id: expectString(value.id),
    displayName: expectString(value.displayName),
    kind: value.kind,
    workspace: value.workspace,
    capabilities: {
      models: evidence(value.capabilities.models),
      resume: evidence(value.capabilities.resume),
      fork: evidence(value.capabilities.fork),
      steer: evidence(value.capabilities.steer),
      followUp: evidence(value.capabilities.followUp),
      permissions: evidence(value.capabilities.permissions),
      questions: evidence(value.capabilities.questions),
      compaction: evidence(value.capabilities.compaction),
    },
    configuration: value.configuration === undefined
      ? unknownRuntimeConfigurationCapabilities()
      : parseRuntimeConfigurationCapabilities(value.configuration),
  };
}

function parseWorkspaceSelection(value: unknown): AppWorkspaceSelection {
  assertRecord(value);
  assertKeys(value, ["cwd", "runtime", "selected"]);
  return {
    cwd: expectString(value.cwd),
    runtime: expectString(value.runtime),
    selected: expectBoolean(value.selected),
  };
}

function parseModelCatalog(value: unknown): AppModelCatalog {
  assertRecord(value);
  assertKeys(value, [
    "provider", "currentModel", "defaultModel", "revision", "fetchedAtMs", "freshness", "warning", "models",
  ]);
  const freshness = value.freshness;
  if (
    freshness !== "live" && freshness !== "fresh_cache" &&
    freshness !== "stale_fallback" && freshness !== "default_fallback"
  ) throw new HeycodeError("invalid_request");
  let warning: AppModelCatalog["warning"] = null;
  if (value.warning !== null) {
    assertRecord(value.warning);
    assertKeys(value.warning, ["code", "message"]);
    warning = { code: expectString(value.warning.code), message: expectString(value.warning.message) };
  }
  return {
    provider: expectString(value.provider),
    currentModel: expectString(value.currentModel),
    defaultModel: expectString(value.defaultModel),
    revision: nullableNumber(value.revision),
    fetchedAtMs: nullableNumber(value.fetchedAtMs),
    freshness,
    warning,
    models: parseArray(value.models).map(parseModel),
  };
}

function parseModel(value: unknown): AppModelRow {
  assertRecord(value);
  assertKeys(value, [
    "id", "displayName", "aliases", "contextWindow", "maxOutputTokens", "lifecycle",
    "selectable", "replacementIds", "capabilities",
  ]);
  const lifecycle = value.lifecycle;
  if (
    lifecycle !== "unknown" && lifecycle !== "stable" && lifecycle !== "preview" &&
    lifecycle !== "deprecated" && lifecycle !== "retired"
  ) throw new HeycodeError("invalid_request");
  return {
    id: expectString(value.id),
    displayName: expectString(value.displayName),
    aliases: stringArray(value.aliases),
    contextWindow: nullableNumber(value.contextWindow),
    maxOutputTokens: nullableNumber(value.maxOutputTokens),
    lifecycle,
    selectable: expectBoolean(value.selectable),
    replacementIds: stringArray(value.replacementIds),
    capabilities: parseModelCapabilities(value.capabilities),
  };
}

function parseModelCapabilities(value: unknown): AppModelCapabilities {
  assertRecord(value);
  assertKeys(value, [
    "tools", "reasoning", "imageInput", "documentInput", "structuredOutput",
    "nativeWeb", "nativeCompaction", "promptCache",
  ]);
  return {
    tools: evidence(value.tools),
    reasoning: evidence(value.reasoning),
    imageInput: evidence(value.imageInput),
    documentInput: evidence(value.documentInput),
    structuredOutput: evidence(value.structuredOutput),
    nativeWeb: evidence(value.nativeWeb),
    nativeCompaction: evidence(value.nativeCompaction),
    promptCache: evidence(value.promptCache),
  };
}

function evidence(value: unknown): AppCapabilityEvidence {
  if (value === "supported" || value === "unsupported" || value === "unknown") return value;
  throw new HeycodeError("invalid_request");
}

function parsePluginInventory(value: unknown): AppPluginInventory {
  assertRecord(value);
  assertKeys(value, ["plugins", "contributions"]);
  return {
    plugins: parseArray(value.plugins).map(plugin => {
      assertRecord(plugin);
      assertKeys(plugin, ["id", "version", "source", "scope"]);
      return {
        id: expectString(plugin.id), version: expectString(plugin.version),
        source: expectString(plugin.source), scope: expectString(plugin.scope),
      };
    }),
    contributions: parseArray(value.contributions).map(row => {
      assertRecord(row);
      assertKeys(row, ["plugin", "kind", "name"]);
      return {
        plugin: expectString(row.plugin), kind: expectString(row.kind), name: expectString(row.name),
      };
    }),
  };
}

function parseSettings(value: unknown): AppSettingsSnapshot {
  assertRecord(value);
  assertKeys(value, [
    "namespace", "exposed", "writable", "applies", "revision", "schema", "defaults",
    "base", "user", "project", "resolved",
  ]);
  return {
    namespace: expectString(value.namespace),
    exposed: expectBoolean(value.exposed),
    writable: expectBoolean(value.writable),
    applies: expectString(value.applies),
    revision: expectNumber(value.revision),
    schema: nullableJson(value.schema),
    defaults: nullableJson(value.defaults),
    base: nullableJson(value.base),
    user: nullableJson(value.user),
    project: nullableJson(value.project),
    resolved: nullableJson(value.resolved),
  };
}

function parseNull(value: unknown): null {
  if (value !== null) throw new HeycodeError("invalid_request");
  return null;
}

function parseArray(value: unknown): unknown[] {
  if (!Array.isArray(value)) throw new HeycodeError("invalid_request");
  return value;
}

function parseJson(raw: string): unknown {
  try { return JSON.parse(raw) as unknown; }
  catch { throw new HeycodeError("invalid_request"); }
}

function parseJsonValue(value: unknown, depth = 0): JsonValue {
  if (depth > 128) throw new HeycodeError("invalid_request");
  if (value === null || typeof value === "string" || typeof value === "boolean") return value;
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (Array.isArray(value)) return value.map(item => parseJsonValue(item, depth + 1));
  assertRecord(value);
  return Object.fromEntries(
    Object.entries(value).map(([key, item]) => [key, parseJsonValue(item, depth + 1)]),
  );
}

function nullableJson(value: unknown): JsonValue | null {
  return value === null ? null : parseJsonValue(value);
}

function nullableString(value: unknown): string | null {
  return value === null ? null : expectString(value);
}

function nullableBoolean(value: unknown): boolean | null {
  return value === null ? null : expectBoolean(value);
}

function optionalBoolean(value: unknown, fallback: boolean): boolean {
  return value === undefined ? fallback : expectBoolean(value);
}

function nullableNumber(value: unknown): number | null {
  return value === null ? null : expectNumber(value);
}

function stringArray(value: unknown): string[] {
  return parseArray(value).map(expectString);
}

function assertRecord(value: unknown): asserts value is Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new HeycodeError("invalid_request");
  }
}

function assertKeys(value: Record<string, unknown>, allowed: readonly string[]): void {
  const accepted = new Set(allowed);
  if (Object.keys(value).some(key => !accepted.has(key))) throw new HeycodeError("invalid_request");
}

function expectString(value: unknown): string {
  if (typeof value !== "string") throw new HeycodeError("invalid_request");
  return value;
}

function expectBoolean(value: unknown): boolean {
  if (typeof value !== "boolean") throw new HeycodeError("invalid_request");
  return value;
}

function expectNumber(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new HeycodeError("invalid_request");
  }
  return value;
}

function assertWireBound(raw: string): void {
  if (raw.length === 0 || new TextEncoder().encode(raw).byteLength > MAX_WIRE_BYTES) {
    throw new HeycodeError("invalid_request");
  }
}

function mapRpcCode(code: number): AppServerErrorCode {
  switch (code) {
    case -32602: return "invalid_request";
    case -32601: return "method_not_found";
    case -32001: return "conflict";
    case -32800: return "cancelled";
    case -32003: return "unsupported";
    case -32004: return "closed";
    case -32603: return "internal";
    default: return "unavailable";
  }
}

function errorMessage(code: AppServerErrorCode): string {
  switch (code) {
    case "invalid_request": return "app-server request is invalid";
    case "method_not_found": return "app-server method not found";
    case "conflict": return "app-server operation conflicts";
    case "cancelled": return "app-server operation was cancelled";
    case "unsupported": return "app-server selection is known but not installed here";
    case "unavailable": return "app-server is unavailable";
    case "closed": return "app-server is closed";
    case "internal": return "app-server operation failed";
  }
}
