import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { resolveCliArgsFromVSCodeExecutablePath, runTests } from "@vscode/test-electron";

const extensionRoot = fileURLToPath(new URL("..", import.meta.url));
const repositoryRoot = fileURLToPath(new URL("../../..", import.meta.url));
const binary = join(repositoryRoot, "target", "debug", "heycode");
const vsix = join(extensionRoot, "dist", "heycode-vscode-0.1.0.vsix");
const harness = join(extensionRoot, "test", "installed-host-harness");
const tests = join(extensionRoot, "test", "installed-host", "index.cjs");
const configured = process.env.HEYCODE_VSCODE_EXECUTABLE_PATH;
const installed = "/Applications/Visual Studio Code.app/Contents/MacOS/Code";
const vscodeExecutablePath = configured ?? (existsSync(installed) ? installed : undefined);
if (vscodeExecutablePath === undefined) {
  throw new Error("set HEYCODE_VSCODE_EXECUTABLE_PATH to run the installed-VSIX journey");
}
if (!existsSync(binary)) throw new Error(`shipping heycode binary is missing: ${binary}`);
if (!existsSync(vsix)) throw new Error(`packaged VSIX is missing: ${vsix}`);

const root = await mkdtemp(join(tmpdir(), "heycode-installed-vsix-"));
const workspace = join(root, "workspace");
const extensions = join(root, "extensions");
const userData = join(root, "user-data");
const home = join(root, "heycode-home");
await Promise.all([mkdir(workspace), mkdir(extensions), mkdir(userData), mkdir(home)]);

const [cli, ...cliArgs] = resolveCliArgsFromVSCodeExecutablePath(vscodeExecutablePath, {
  reuseMachineInstall: true,
});
try {
  const installedResult = spawnSync(
    cli,
    [
      ...cliArgs,
      `--extensions-dir=${extensions}`,
      `--user-data-dir=${userData}`,
      "--install-extension",
      vsix,
      "--force",
    ],
    { encoding: "utf8", env: process.env, shell: false },
  );
  if (installedResult.status !== 0) {
    throw new Error("isolated VSIX installation failed");
  }
  await runTests({
    vscodeExecutablePath,
    extensionDevelopmentPath: harness,
    extensionTestsPath: tests,
    reuseMachineInstall: true,
    launchArgs: [
      workspace,
      `--extensions-dir=${extensions}`,
      `--user-data-dir=${userData}`,
    ],
    extensionTestsEnv: {
      HEYCODE_HOME: home,
      HEYCODE_TEST_BINARY: binary,
      HEYCODE_INSTALLED_EXTENSION_ROOT: extensions,
    },
  });
} finally {
  await rm(root, { recursive: true, force: true });
}
