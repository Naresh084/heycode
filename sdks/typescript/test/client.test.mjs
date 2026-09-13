import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  APP_SERVER_PROTOCOL_VERSION,
  HeycodeClient,
  HeycodeError,
  decodeNotification,
} from "../dist/client.js";

class ScriptedTransport {
  #cancel;
  #cancelled = new Promise(resolve => { this.#cancel = resolve; });
  requests = [];

  async exchange(raw, onNotification) {
    const request = JSON.parse(raw);
    this.requests.push(request);
    let result;
    switch (request.method) {
      case "initialize":
        result = {
          protocolVersion: APP_SERVER_PROTOCOL_VERSION,
          server: { name: "heycode", version: "0.1.0" },
          capabilities: {
            turns: true, attachments: true, cancel: true, authorization: true,
            models: true, mcp: true, plugins: true, settings: true,
          },
        };
        break;
      case "session/open":
        result = sessionInfo(request.params.configuration);
        break;
      case "session/configure":
        result = sessionInfo(request.params.configuration);
        break;
      case "runtime/models":
        result = [{
          model: "runtime-model",
          defaultReasoningEffort: "medium",
          reasoningEfforts: ["low", "medium", "high"],
        }];
        break;
      case "turn/start":
        await onNotification(JSON.stringify({
          jsonrpc: "2.0",
          method: "session/event",
          params: {
            sessionId: "session-1",
            sequence: 90,
            event: { type: "turn_started", turn_id: "7" },
          },
        }));
        await this.#cancelled;
        await onNotification(JSON.stringify({
          jsonrpc: "2.0",
          method: "session/event",
          params: {
            sessionId: "session-1",
            sequence: 91,
            event: { type: "turn_finished", turn_id: "7", reason: "cancelled" },
          },
        }));
        result = { turnId: "7", reason: "cancelled" };
        break;
      case "turn/cancel":
        this.#cancel();
        result = null;
        break;
      case "session/permission/respond":
      case "session/question/respond":
        result = null;
        break;
      case "settings/list":
        result = [{
          namespace: "routing", exposed: true, writable: true, applies: "live",
          revision: 0, schema: {}, defaults: {}, base: null, user: null,
          project: null, resolved: { runtime: "native", provider: "fake", model: "fake" },
        }];
        break;
      default:
        throw new Error(`unexpected method ${request.method}`);
    }
    return JSON.stringify({ jsonrpc: "2.0", id: request.id, result });
  }
}

function sessionInfo(configuration = {}) {
  return {
    sessionId: "session-1",
    runtimeId: "native",
    cwd: "/workspace",
    configuration,
    configurationCapabilities: {
      systemPrompt: "supported",
      tools: "supported",
      model: "supported",
      reasoningEffort: "supported",
    },
  };
}

class LegacySessionTransport {
  async exchange(raw) {
    const request = JSON.parse(raw);
    const result = request.method === "initialize"
      ? {
          protocolVersion: APP_SERVER_PROTOCOL_VERSION,
          server: { name: "heycode", version: "0.1.0" },
          capabilities: {
            turns: true, attachments: true, cancel: true, authorization: false,
            models: false, mcp: false, plugins: false, settings: false,
          },
        }
      : { sessionId: "legacy-session", runtimeId: "legacy-runtime", cwd: "/workspace" };
    return JSON.stringify({ jsonrpc: "2.0", id: request.id, result });
  }
}

test("older session rows default additive runtime controls conservatively", async () => {
  const session = await new HeycodeClient(new LegacySessionTransport()).start();
  assert.deepEqual(session.configuration, {});
  assert.deepEqual(session.configurationCapabilities, {
    systemPrompt: "unknown",
    tools: "unknown",
    model: "unknown",
    reasoningEffort: "unknown",
  });
});

test("typed client starts, resumes, streams, cancels, and reads controls", async () => {
  const transport = new ScriptedTransport();
  const client = new HeycodeClient(transport);
  const started = await client.start();
  assert.equal(started.sessionId, "session-1");
  assert.deepEqual(await client.resume("session-1"), started);
  await client.respondPermission("request-1", "allow_once");
  await client.respondQuestion("request-2", "Yes");
  await client.cancelQuestion("request-3");
  assert.deepEqual(
    transport.requests.find(request => request.params?.requestId === "request-2").params,
    { sessionId: "session-1", requestId: "request-2", answer: "Yes" },
  );
  assert.deepEqual(
    transport.requests.find(request => request.params?.requestId === "request-3").params,
    { sessionId: "session-1", requestId: "request-3", cancelled: true },
  );

  const events = [];
  const turn = client.turn("wait", [], event => { events.push(event); });
  while (events.length === 0) await new Promise(resolve => setImmediate(resolve));
  assert.equal(events[0].params.event.type, "turn_started");
  await client.cancel();
  assert.equal((await turn).reason, "cancelled");
  assert.equal(events[1].params.sequence, 91);
  assert.equal(events[1].params.event.type, "turn_finished");

  const settings = await client.settings();
  assert.equal(settings[0].namespace, "routing");
  assert.equal(settings[0].exposed, true);
});

test("runtime configuration and model choices round-trip with exact request shapes", async () => {
  const transport = new ScriptedTransport();
  const client = new HeycodeClient(transport);
  const configuration = {
    systemPrompt: "Use exact instructions",
    tools: [{
      name: "read_file",
      description: "Read one file",
      parameters: { type: "object" },
    }],
    model: "runtime-model",
    reasoningEffort: "high",
  };
  const opened = await client.startWithConfiguration(configuration);
  assert.deepEqual(opened.configuration, configuration);
  assert.equal(opened.configurationCapabilities.tools, "supported");
  assert.deepEqual(
    transport.requests.find(request => request.method === "session/open").params,
    { configuration },
  );
  const configured = await client.configure({ tools: [] });
  assert.deepEqual(configured.configuration, { tools: [] });
  assert.deepEqual(
    transport.requests.find(request => request.method === "session/configure").params,
    { sessionId: "session-1", configuration: { tools: [] } },
  );
  assert.deepEqual(await client.runtimeModels(), [{
    model: "runtime-model",
    defaultReasoningEffort: "medium",
    reasoningEfforts: ["low", "medium", "high"],
  }]);
});

class GapTransport extends ScriptedTransport {
  async exchange(raw, onNotification) {
    const request = JSON.parse(raw);
    if (request.method !== "turn/start") return super.exchange(raw, onNotification);
    for (const sequence of [3, 5]) {
      await onNotification(JSON.stringify({
        jsonrpc: "2.0",
        method: "session/event",
        params: {
          sessionId: "session-1",
          sequence,
          event: { type: "notice", code: "test", message: "safe" },
        },
      }));
    }
    return JSON.stringify({
      jsonrpc: "2.0",
      id: request.id,
      result: { turnId: "1", reason: "stop" },
    });
  }
}

test("notification sequence gaps fail before a successful response is returned", async () => {
  const client = new HeycodeClient(new GapTransport());
  await client.start();
  await assert.rejects(
    client.turn("hello", [], () => {}),
    error => error instanceof HeycodeError && error.code === "invalid_request",
  );
});

test("shared Rust/TypeScript v1 fixture decodes baseline closed events", () => {
  const fixture = JSON.parse(readFileSync(
    new URL("../../fixtures/app-server-v1.json", import.meta.url),
    "utf8",
  ));
  assert.equal(fixture.protocolVersion, APP_SERVER_PROTOCOL_VERSION);
  const notifications = fixture.notifications.map(value => decodeNotification(JSON.stringify(value)));
  assert.equal(notifications.length, 15);
  assert.deepEqual(
    notifications.map(notification => notification.params.event.type),
    [
      "user_input", "turn_started", "assistant_delta", "reasoning_delta",
      "tool_started", "tool_finished", "usage", "plan_changed", "notice",
      "authorization_prompt_requested", "authorization_prompt_resolved",
      "permission_requested", "question_requested", "assistant_audio",
      "turn_finished",
    ],
  );
  const usage = notifications.find(notification => notification.params.event.type === "usage");
  assert.deepEqual(usage.params.event.context, { tokens: 2767, context_window: 1000000, resolved_model: "claude-opus-5[1m]" });
  assert.equal(fixture.authorization.credential.inspected, false);
  assert.equal(fixture.settings.exposed, true);
});

test("question notification preserves optional header and aligned descriptions", () => {
  const notification = decodeNotification(JSON.stringify({
    jsonrpc: "2.0",
    method: "session/event",
    params: {
      sessionId: "session-1",
      sequence: 1,
      event: {
        type: "question_requested",
        request_id: "question-1",
        header: "Intent",
        prompt: "What should I build?",
        choices: ["Dashboard", "Report"],
        choice_descriptions: ["Build the dashboard", null],
      },
    },
  }));
  assert.deepEqual(notification.params.event, {
    type: "question_requested",
    request_id: "question-1",
    header: "Intent",
    prompt: "What should I build?",
    choices: ["Dashboard", "Report"],
    choice_descriptions: ["Build the dashboard", null],
  });
});

test("shared v1 initialize fixture preserves optional runtime and workspace capabilities", async () => {
  const fixture = JSON.parse(readFileSync(
    new URL("../../fixtures/app-server-v1.json", import.meta.url),
    "utf8",
  ));
  const transport = {
    async exchange(raw) {
      const request = JSON.parse(raw);
      return JSON.stringify({ jsonrpc: "2.0", id: request.id, result: fixture.initialize });
    },
  };
  const initialized = await new HeycodeClient(transport).initialize();
  assert.equal(initialized.capabilities.runtimes, false);
  assert.equal(initialized.capabilities.workspace, false);
});

test("audio output decodes metadata without any raw-byte field", () => {
  const raw = JSON.stringify({
    jsonrpc: "2.0",
    method: "session/event",
    params: {
      sessionId: "session-1",
      sequence: 7,
      event: {
        type: "assistant_audio",
        attachments: [{
          content_id: `sha256-${"ab".repeat(32)}`,
          media_type: "audio/wav",
          byte_len: 16044,
          display_name: "answer.wav",
          audio: {
            duration_ms: 1000,
            sample_rate_hz: 8000,
            channels: 1,
            bits_per_sample: 16,
          },
        }],
      },
    },
  });
  const decoded = decodeNotification(raw);
  assert.equal(decoded.params.event.type, "assistant_audio");
  assert.equal(decoded.params.event.attachments[0].audio.duration_ms, 1000);
  assert.equal("data" in decoded.params.event.attachments[0], false);
  assert.equal("bytes" in decoded.params.event.attachments[0], false);

  const hostile = JSON.parse(raw);
  hostile.params.event.attachments[0].data = "UkFXLUFVRElP";
  assert.throws(
    () => decodeNotification(JSON.stringify(hostile)),
    error => error instanceof HeycodeError && error.code === "invalid_request",
  );
});
