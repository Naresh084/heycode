import { existsSync } from "node:fs";
import { mkdtemp, mkdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { runTests } from "@vscode/test-electron";

const extensionDevelopmentPath = fileURLToPath(new URL("..", import.meta.url));
const extensionTestsPath = fileURLToPath(new URL("../test/extension-host/index.cjs", import.meta.url));
const root = await mkdtemp(join(tmpdir(), "heycode-vscode-host-"));
const workspace = join(root, "workspace");
await mkdir(workspace);

const configured = process.env.HEYCODE_VSCODE_EXECUTABLE_PATH;
const installed = "/Applications/Visual Studio Code.app/Contents/MacOS/Code";
const vscodeExecutablePath = configured ?? (existsSync(installed) ? installed : undefined);

try {
  await runTests({
    extensionDevelopmentPath,
    extensionTestsPath,
    ...(vscodeExecutablePath === undefined ? {} : { vscodeExecutablePath }),
    reuseMachineInstall: false,
    launchArgs: [workspace, "--disable-extensions", "--skip-welcome"],
  });
} finally {
  await rm(root, { recursive: true, force: true });
}
