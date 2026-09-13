import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { HeycodeSessionController, StdioAppTransport } from "../dist/testing.mjs";

const fixture = fileURLToPath(new URL("fixture-host.mjs", import.meta.url));

function host() {
  return new StdioAppTransport({
    executable: process.execPath,
    args: [fixture],
    cwd: fileURLToPath(new URL("..", import.meta.url)),
  });
}

test("one child host session resumes and forwards one exact permission choice", async () => {
  const transport = host();
  const permissions = [];
  const events = [];
  const controller = new HeycodeSessionController(transport, {
    onEvent(event) { events.push(event.params.event.type); },
    async choosePermission(request) {
      permissions.push(request.request_id);
      return "allow_once";
    },
    async answerQuestion() { return null; },
  });

  const session = await controller.start();
  assert.equal(session.sessionId, "vscode-session-1");
  assert.equal((await controller.resume(session.sessionId)).sessionId, session.sessionId);
  const result = await controller.send("permission");
  assert.equal(result.reason, "stop");
  assert.deepEqual(permissions, ["permission-7"]);
  assert.deepEqual(events, ["permission_requested", "assistant_delta", "turn_finished"]);
  await controller.close();
  assert.equal(transport.closed, true);
});

test("permission denial uses the exact correlated request without poisoning the session", async () => {
  const transport = host();
  const permissions = [];
  const controller = new HeycodeSessionController(transport, {
    onEvent() {},
    async choosePermission(request) {
      permissions.push(request.request_id);
      return "deny";
    },
    async answerQuestion() { return null; },
  });
  await controller.start();
  assert.equal((await controller.send("permission-deny")).reason, "stop");
  assert.deepEqual(permissions, ["permission-7"]);
  assert.equal((await controller.send("healthy")).reason, "stop");
  await controller.close();
});

test("cancel is a separately awaited app-server operation and the next turn remains healthy", async () => {
  const transport = host();
  let started;
  const startedEvent = new Promise(resolve => { started = resolve; });
  const controller = new HeycodeSessionController(transport, {
    onEvent(event) {
      if (event.params.event.type === "turn_started") started();
    },
    async choosePermission() { return "deny"; },
    async answerQuestion() { return null; },
  });
  await controller.start();
  const turn = controller.send("cancel");
  await startedEvent;
  await Promise.all([controller.cancel(), controller.cancel()]);
  assert.equal((await turn).reason, "cancelled");
  assert.equal((await controller.send("healthy")).reason, "stop");
  await controller.close();
});

test("transport discards child stderr and exposes only a body-free failure", async () => {
  const transport = new StdioAppTransport({
    executable: process.execPath,
    args: [fixture, "--fail-with-stderr"],
    cwd: fileURLToPath(new URL("..", import.meta.url)),
  });
  const controller = new HeycodeSessionController(transport, {
    onEvent() {},
    async choosePermission() { return "deny"; },
    async answerQuestion() { return null; },
  });
  await assert.rejects(
    controller.start(),
    error => error instanceof Error && !error.message.includes("authorization-secret-canary"),
  );
  await controller.dispose();
});

test("extension manifest is installable, explicit, and activation never starts a host", async () => {
  const manifest = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  assert.equal(manifest.main, "./dist/extension.cjs");
  assert.equal(manifest.extensionKind[0], "workspace");
  assert.equal(manifest.contributes.configuration.properties["heycode.executablePath"].default, "");
  assert.deepEqual(
    manifest.contributes.commands.map(row => row.command),
    [
      "heycode.startSession", "heycode.resumeSession", "heycode.sendMessage",
      "heycode.cancelTurn", "heycode.closeSession", "heycode.showOutput",
    ],
  );
});

test("transport source has no raw-frame or child-stderr logging path", async () => {
  const transport = await readFile(new URL("../src/transport.ts", import.meta.url), "utf8");
  assert.doesNotMatch(transport, /console\./u);
  assert.doesNotMatch(transport, /OutputChannel/u);
  assert.match(transport, /stderr\.on\("data", \(\) => undefined\)/u);
});

test("shared stdio v1 fixture locks operation and inner JSON-RPC correlation", async () => {
  const fixture = JSON.parse(await readFile(
    new URL("../../../sdks/fixtures/app-server-stdio-v1.json", import.meta.url),
    "utf8",
  ));
  assert.equal(fixture.transportVersion, 1);
  assert.equal(fixture.request.operation, fixture.request.request.id);
  assert.equal(fixture.notification.operation, fixture.request.operation);
  assert.equal(fixture.response.operation, fixture.response.response.id);
  assert.equal(fixture.cancel.operation, fixture.request.operation);
  assert.equal(
    fixture.notification.notification.params.event.request_id,
    "permission-7",
  );
});
