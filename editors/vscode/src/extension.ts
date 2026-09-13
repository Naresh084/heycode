import { realpath, stat } from "node:fs/promises";
import { isAbsolute } from "node:path";

import {
  HeycodeError,
  type AppPermissionDecision,
  type AppServerNotification,
  type AppSessionInfo,
  type AppTurnReason,
  type AppTurnResult,
} from "@heycode/sdk";
import * as vscode from "vscode";

import { HeycodeSessionController, type HeycodeSessionUi } from "./session.js";
import { StdioAppTransport } from "./transport.js";

const SESSION_KEY = "heycode.sessionId";
let active: HeycodeSessionController | null = null;
let connecting: Promise<AppSessionInfo | null> | null = null;
let closing = false;
let output: vscode.OutputChannel | null = null;
let status: vscode.StatusBarItem | null = null;
let permissionRequests = 0;
let questionRequests = 0;
let lastTurnReason: AppTurnReason | null = null;

export interface HeycodeExtensionSnapshot {
  connected: boolean;
  turnActive: boolean;
  permissionRequests: number;
  questionRequests: number;
  lastTurnReason: AppTurnReason | null;
}

export interface HeycodeExtensionApi {
  snapshot(): HeycodeExtensionSnapshot;
  send(text: string): Promise<AppTurnResult>;
  cancel(): Promise<HeycodeCancelOutcome>;
}

export type HeycodeCancelOutcome =
  | { state: "not_active" }
  | { state: "settled" }
  | { state: "failed"; code: string };

export function activate(context: vscode.ExtensionContext): HeycodeExtensionApi {
  output = vscode.window.createOutputChannel("heycode");
  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 20);
  status.text = "heycode: disconnected";
  status.command = "heycode.showOutput";
  status.show();
  context.subscriptions.push(output, status);

  register(context, "heycode.startSession", () => beginConnect(context, false));
  register(context, "heycode.resumeSession", () => beginConnect(context, true));
  register(context, "heycode.sendMessage", sendMessage);
  register(context, "heycode.cancelTurn", cancelTurn);
  register(context, "heycode.closeSession", closeSession);
  register(context, "heycode.showOutput", () => output?.show(true));
  context.subscriptions.push({ dispose: () => { void closeSession(); } });
  return {
    snapshot: () => ({
      connected: active !== null,
      turnActive: active?.turnActive ?? false,
      permissionRequests,
      questionRequests,
      lastTurnReason,
    }),
    send: text => {
      const controller = active;
      if (controller === null) throw new HeycodeError("conflict");
      return controller.send(text);
    },
    cancel: cancelTurn,
  };
}

export async function deactivate(): Promise<void> {
  await closeSession();
}

function register(
  context: vscode.ExtensionContext,
  command: string,
  callback: () => unknown,
): void {
  context.subscriptions.push(vscode.commands.registerCommand(command, callback));
}

async function beginConnect(
  context: vscode.ExtensionContext,
  resume: boolean,
): Promise<AppSessionInfo | null> {
  if (active !== null || connecting !== null || closing) {
    void vscode.window.showWarningMessage("A heycode session is already connected or changing state.");
    return null;
  }
  const task = connect(context, resume);
  connecting = task;
  try {
    return await task;
  } finally {
    if (connecting === task) connecting = null;
  }
}

async function connect(
  context: vscode.ExtensionContext,
  resume: boolean,
): Promise<AppSessionInfo | null> {
  let transport: StdioAppTransport | null = null;
  let controller: HeycodeSessionController | null = null;
  try {
    const host = await resolveHost(context, resume);
    transport = new StdioAppTransport(host);
    controller = new HeycodeSessionController(transport, vscodeUi());
    const session = resume
      ? await controller.resume(host.expectedSessionId as string)
      : await controller.start();
    await context.workspaceState.update(SESSION_KEY, session.sessionId);
    active = controller;
    if (status !== null) status.text = `heycode: ${session.runtimeId}`;
    output?.appendLine(`Connected to heycode session ${session.sessionId}.`);
    output?.show(true);
    return session;
  } catch (error: unknown) {
    await controller?.dispose();
    if (controller === null) await transport?.dispose();
    void vscode.window.showErrorMessage(safeMessage(error));
    return null;
  }
}

async function sendMessage(message?: unknown): Promise<AppTurnResult | null> {
  const controller = active;
  if (controller === null) {
    void vscode.window.showWarningMessage("Start or resume a heycode session first.");
    return null;
  }
  if (controller.turnActive) {
    void vscode.window.showWarningMessage("A heycode turn is already active.");
    return null;
  }
  const text = typeof message === "string"
    ? message
    : await vscode.window.showInputBox({
        title: "Send to heycode",
        prompt: "Message",
        ignoreFocusOut: true,
        validateInput: value => value.length === 0 ? "Enter a message." : undefined,
      });
  if (text === undefined || text.length === 0) return null;
  if (status !== null) status.text = "heycode: working";
  output?.appendLine("You: message admitted to heycode.");
  try {
    const result = await controller.send(text);
    output?.appendLine(`Turn settled: ${result.reason}.`);
    return result;
  } catch (error: unknown) {
    void vscode.window.showErrorMessage(safeMessage(error));
    return null;
  } finally {
    if (status !== null) status.text = `heycode: ${controller.session?.runtimeId ?? "connected"}`;
  }
}

async function cancelTurn(): Promise<HeycodeCancelOutcome> {
  const controller = active;
  if (controller === null || !controller.turnActive) return { state: "not_active" };
  try {
    await controller.cancel();
    return { state: "settled" };
  } catch (error: unknown) {
    void vscode.window.showErrorMessage(safeMessage(error));
    return {
      state: "failed",
      code: error instanceof HeycodeError ? error.code : "internal",
    };
  }
}

async function closeSession(): Promise<void> {
  if (closing) return;
  closing = true;
  try {
    await connecting?.catch(() => undefined);
    const controller = active;
    active = null;
    if (controller !== null) await controller.close();
    if (status !== null) status.text = "heycode: disconnected";
  } finally {
    closing = false;
  }
}

function vscodeUi(): HeycodeSessionUi {
  return {
    onEvent: renderEvent,
    choosePermission: async request => {
      const allowOnce = "Allow once";
      const allowSession = "Allow for session";
      const deny = "Deny";
      const selected = await vscode.window.showWarningMessage(
        request.action,
        { modal: true, detail: request.detail },
        allowOnce,
        allowSession,
        deny,
      );
      const choices: Record<string, AppPermissionDecision> = {
        [allowOnce]: "allow_once",
        [allowSession]: "allow_session",
        [deny]: "deny",
      };
      return selected === undefined ? "deny" : choices[selected] ?? "deny";
    },
    answerQuestion: async request => {
      if (request.choices.length === 0) {
        return (await vscode.window.showInputBox({
          title: request.prompt,
          ignoreFocusOut: true,
        })) ?? null;
      }
      return (await vscode.window.showQuickPick(request.choices, {
        title: request.prompt,
        ignoreFocusOut: true,
      })) ?? null;
    },
  };
}

function renderEvent(notification: AppServerNotification): void {
  const event = notification.params.event;
  switch (event.type) {
    case "assistant_delta":
      output?.append(event.text);
      break;
    case "reasoning_delta":
      output?.appendLine("[reasoning update]");
      break;
    case "tool_started":
      output?.appendLine(`Tool started: ${event.name}.`);
      break;
    case "tool_finished":
      output?.appendLine(`Tool finished: ${event.name} (${event.ok ? "ok" : "error"}).`);
      break;
    case "permission_requested":
      permissionRequests += 1;
      output?.appendLine(`Permission requested: ${event.action}.`);
      break;
    case "question_requested":
      questionRequests += 1;
      output?.appendLine("heycode requested an answer.");
      break;
    case "notice":
      output?.appendLine(`Notice: ${event.code}.`);
      break;
    case "turn_finished":
      lastTurnReason = event.reason;
      output?.appendLine(`Turn finished: ${event.reason}.`);
      break;
    case "user_input":
    case "turn_started":
    case "usage":
    case "plan_changed":
    case "authorization_prompt_requested":
    case "authorization_prompt_resolved":
      break;
  }
}

async function resolveHost(
  context: vscode.ExtensionContext,
  resume: boolean,
): Promise<{
  executable: string;
  args: string[];
  cwd: string;
  env: NodeJS.ProcessEnv;
  expectedSessionId?: string;
}> {
  const folder = vscode.workspace.workspaceFolders?.[0];
  if (folder === undefined) throw new HeycodeError("invalid_request");
  const configuration = vscode.workspace.getConfiguration("heycode", folder.uri);
  const configured = configuration.get<string>("executablePath", "").trim();
  if (!isAbsolute(configured)) throw new HeycodeError("invalid_request");
  const executable = await realpath(configured).catch(() => { throw new HeycodeError("unavailable"); });
  const metadata = await stat(executable).catch(() => { throw new HeycodeError("unavailable"); });
  if (!metadata.isFile()) throw new HeycodeError("invalid_request");
  const hostArguments = configuration.get<unknown>("hostArguments");
  if (
    !Array.isArray(hostArguments) ||
    hostArguments.length > 128 ||
    hostArguments.some(value => typeof value !== "string" || value.length > 4096 || value.includes("\0"))
  ) {
    throw new HeycodeError("invalid_request");
  }
  const cwd = folder.uri.fsPath;
  const args = [...hostArguments, "--workspace", cwd];
  const expectedSessionId = resume ? context.workspaceState.get<string>(SESSION_KEY) : undefined;
  if (resume && expectedSessionId === undefined) throw new HeycodeError("invalid_request");
  if (expectedSessionId !== undefined) args.push("--resume", expectedSessionId);
  return { executable, args, cwd, env: process.env, ...(expectedSessionId === undefined ? {} : { expectedSessionId }) };
}

function safeMessage(error: unknown): string {
  return error instanceof HeycodeError ? error.message : "heycode operation failed.";
}
