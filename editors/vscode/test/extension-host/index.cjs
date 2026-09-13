const assert = require("node:assert/strict");
const vscode = require("vscode");

async function run() {
  const extension = vscode.extensions.getExtension("heycode.heycode-vscode");
  assert.ok(extension, "packaged extension must be discoverable");
  await extension.activate();
  const commands = await vscode.commands.getCommands(true);
  for (const command of [
    "heycode.startSession",
    "heycode.resumeSession",
    "heycode.sendMessage",
    "heycode.cancelTurn",
    "heycode.closeSession",
    "heycode.showOutput",
  ]) {
    assert.ok(commands.includes(command), `${command} must be registered`);
  }
  const configured = vscode.workspace.getConfiguration("heycode").get("executablePath");
  assert.equal(configured, "", "activation must not implicitly select or start a host executable");
  await vscode.commands.executeCommand("heycode.showOutput");
}

module.exports = { run };
