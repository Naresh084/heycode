const assert = require("node:assert/strict");
const { realpath } = require("node:fs/promises");
const path = require("node:path");
const vscode = require("vscode");

async function run() {
  const binary = process.env.HEYCODE_TEST_BINARY;
  const installedRoot = process.env.HEYCODE_INSTALLED_EXTENSION_ROOT;
  assert.ok(binary, "HEYCODE_TEST_BINARY must name the shipping heycode binary");
  assert.ok(installedRoot, "HEYCODE_INSTALLED_EXTENSION_ROOT must name the isolated extension root");

  const configuration = vscode.workspace.getConfiguration("heycode");
  await configuration.update("executablePath", binary, vscode.ConfigurationTarget.Global);
  await configuration.update(
    "hostArguments",
    ["--restricted-workspace", "--fake", "app-server", "--stdio-v1"],
    vscode.ConfigurationTarget.Global,
  );

  const extension = vscode.extensions.getExtension("heycode.heycode-vscode");
  assert.ok(extension, "the installed heycode VSIX must be discoverable");
  assert.equal(extension.packageJSON.version, "0.1.0");
  const extensionPath = await realpath(extension.extensionPath);
  const expectedRoot = await realpath(installedRoot);
  assert.ok(
    extensionPath.startsWith(`${expectedRoot}${path.sep}`),
    `heycode must load from the isolated installed VSIX root: ${extensionPath}`,
  );
  assert.equal(extension.isActive, false, "configuration must not activate the extension");

  const started = await vscode.commands.executeCommand("heycode.startSession");
  assert.ok(started, "Start Session must return the opened app-server session");
  assert.equal(started.runtimeId, "native");
  assert.equal(await realpath(started.cwd), await realpath(vscode.workspace.workspaceFolders[0].uri.fsPath));

  const api = extension.exports;
  assert.equal(typeof api?.snapshot, "function", "installed extension must expose its safe test snapshot");
  assert.equal(api.snapshot().connected, true);

  const cancellingTurn = api.send(`cancel-race ${"x".repeat(900 * 1024)}`).then(
    result => ({ result, error: null }),
    error => ({ result: null, error: error?.code ?? "internal" }),
  );
  const activeObserved = api.snapshot().turnActive;
  assert.equal(activeObserved, true, "the cancel command must overlap an admitted turn");
  const cancelOutcome = await api.cancel();
  const cancelTurn = await cancellingTurn;
  assert.notEqual(cancelOutcome.state, "not_active");
  assert.ok(
    cancelTurn.result?.reason === "cancelled" ||
      cancelTurn.result?.reason === "stop" ||
      cancelTurn.error === "cancelled",
    `fake turn must settle or cancel after the concurrent request: ${JSON.stringify(cancelTurn)}`,
  );

  const beforeClose = api.snapshot();
  assert.equal(beforeClose.permissionRequests, 0);
  assert.equal(beforeClose.questionRequests, 0);

  await vscode.commands.executeCommand("heycode.closeSession");
  assert.equal(api.snapshot().connected, false);
  const resumed = await vscode.commands.executeCommand("heycode.resumeSession");
  assert.equal(resumed?.sessionId, started.sessionId, "Resume must bind the exact UUID session");
  const resumedTurn = await vscode.commands.executeCommand(
    "heycode.sendMessage",
    "healthy operation after concurrent cancel and exact resume",
  );
  assert.equal(resumedTurn?.reason, "stop");
  await vscode.commands.executeCommand("heycode.closeSession");
  assert.equal(api.snapshot().connected, false);

  process.stdout.write(`HEYCODE_INSTALLED_VSIX_JOURNEY ${JSON.stringify({
    extension: "heycode.heycode-vscode@0.1.0",
    runtime: started.runtimeId,
    cancelOverlapped: activeObserved,
    cancelOutcome,
    cancelTurn: cancelTurn.result?.reason ?? null,
    cancelError: cancelTurn.error,
    permissionRequests: beforeClose.permissionRequests,
    questionRequests: beforeClose.questionRequests,
    exactResume: resumed.sessionId === started.sessionId,
    healthyAfterCancelAndResume: resumedTurn.reason,
    closed: api.snapshot().connected === false,
  })}\n`);
}

module.exports = { run };
