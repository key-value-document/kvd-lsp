import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig } from "@vscode/test-cli";

const root = path.dirname(fileURLToPath(import.meta.url));
const server = path.resolve(
    root,
    "..",
    "..",
    "target",
    "debug",
    process.platform === "win32" ? "kvd-lsp.exe" : "kvd-lsp"
);

export default defineConfig({
    env: { KVD_LSP_PATH: server },
    files: "out/test/**/*.test.js",
    launchArgs: ["--disable-extensions"],
    mocha: {
        timeout: 20000,
        ui: "tdd",
    },
});
