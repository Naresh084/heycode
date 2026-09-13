import { build } from "esbuild";
import { mkdir } from "node:fs/promises";

await mkdir(new URL("../dist/", import.meta.url), { recursive: true });

await build({
  entryPoints: [new URL("../src/extension.ts", import.meta.url).pathname],
  outfile: new URL("../dist/extension.cjs", import.meta.url).pathname,
  bundle: true,
  platform: "node",
  format: "cjs",
  target: "node20",
  external: ["vscode"],
  sourcemap: false,
  logLevel: "warning",
});

await build({
  entryPoints: [new URL("../src/testing.ts", import.meta.url).pathname],
  outfile: new URL("../dist/testing.mjs", import.meta.url).pathname,
  bundle: true,
  platform: "node",
  format: "esm",
  target: "node20",
  sourcemap: false,
  logLevel: "warning",
});
