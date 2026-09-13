import { createInterface } from "node:readline";

const failWithStderr = process.argv.includes("--fail-with-stderr");
if (failWithStderr) {
  process.stderr.write("authorization-secret-canary\n");
  process.exit(23);
}

const sessionId = "vscode-session-1";
let pendingTurn = null;
let permissionSettled = false;
let expectedPermissionDecision = null;
let cancelCount = 0;
let sequence = 40;

function write(frame) {
  process.stdout.write(`${JSON.stringify(frame)}\n`);
}

function response(operation, id, result) {
  write({ kind: "response", operation, response: { jsonrpc: "2.0", id, result } });
}

function error(operation, id, code, message) {
  write({ kind: "response", operation, response: { jsonrpc: "2.0", id, error: { code, message } } });
}

function notification(operation, event) {
  write({
    kind: "notification",
    operation,
    notification: {
      jsonrpc: "2.0",
      method: "session/event",
      params: { sessionId, sequence: sequence++, event },
    },
  });
}

function finishTurn(reason, text) {
  const pending = pendingTurn;
  if (pending === null) return;
  if (text !== null) notification(pending.operation, { type: "assistant_delta", text });
  notification(pending.operation, { type: "turn_finished", turn_id: pending.turnId, reason });
  response(pending.operation, pending.id, { turnId: pending.turnId, reason });
  pendingTurn = null;
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
lines.on("line", raw => {
  const envelope = JSON.parse(raw);
  if (envelope.kind === "cancel") {
    if (pendingTurn?.operation === envelope.operation) finishTurn("cancelled", null);
    return;
  }
  const { operation, request } = envelope;
  const { id, method, params } = request;
  switch (method) {
    case "initialize":
      response(operation, id, {
        protocolVersion: 1,
        server: { name: "heycode", version: "0.1.0" },
        capabilities: {
          turns: true, attachments: true, cancel: true, authorization: false,
          models: false, runtimes: false, workspace: false, mcp: false,
          plugins: false, settings: false,
        },
      });
      break;
    case "session/open":
      response(operation, id, { sessionId, runtimeId: "native", cwd: process.cwd() });
      break;
    case "turn/start": {
      if (pendingTurn !== null) {
        error(operation, id, -32001, "app-server operation conflicts");
        break;
      }
      pendingTurn = { operation, id, turnId: `turn-${id}` };
      if (params.text === "permission" || params.text === "permission-deny") {
        permissionSettled = false;
        expectedPermissionDecision = params.text === "permission" ? "allow_once" : "deny";
        notification(operation, {
          type: "permission_requested",
          request_id: "permission-7",
          action: "Run fixture",
          detail: "Exact IDE choice required",
        });
      } else if (params.text === "cancel") {
        notification(operation, { type: "turn_started", turn_id: pendingTurn.turnId });
      } else {
        finishTurn("stop", "healthy");
      }
      break;
    }
    case "session/permission/respond":
      if (
        permissionSettled || params.sessionId !== sessionId ||
        params.requestId !== "permission-7" || params.decision !== expectedPermissionDecision
      ) {
        error(operation, id, -32001, "app-server operation conflicts");
        break;
      }
      permissionSettled = true;
      expectedPermissionDecision = null;
      response(operation, id, null);
      finishTurn("stop", "approved");
      break;
    case "turn/cancel":
      cancelCount += 1;
      if (cancelCount > 1) {
        error(operation, id, -32001, "app-server operation conflicts");
        break;
      }
      response(operation, id, null);
      finishTurn("cancelled", null);
      break;
    case "session/close":
      response(operation, id, null);
      break;
    default:
      error(operation, id, -32601, "app-server method not found");
  }
});
