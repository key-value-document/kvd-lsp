import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import * as vscode from "vscode";
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

/** Directory where `cargo install` places binaries. */
function cargoBinDir(): string {
    return process.env.CARGO_HOME
        ? path.join(process.env.CARGO_HOME, "bin")
        : path.join(os.homedir(), ".cargo", "bin");
}

/** Default server locations, most preferred first. */
function serverCandidates(context: vscode.ExtensionContext): string[] {
    const exe = process.platform === "win32" ? "kvd-lsp.exe" : "kvd-lsp";
    return [
        path.join(cargoBinDir(), exe),
        path.join(context.extensionPath, "..", "..", "target", "debug", exe),
    ];
}

/** First existing candidate, or the preferred `cargo install` location. */
function defaultServerPath(context: vscode.ExtensionContext): string {
    const candidates = serverCandidates(context);
    return (
        candidates.find((candidate) => fs.existsSync(candidate)) ??
        candidates[0]
    );
}

/** Settings key holding this host's server path, if the OS has one. */
function osServerPathKey(): string | undefined {
    switch (process.platform) {
        case "linux":
            return "serverPathLinux";
        case "darwin":
            return "serverPathDarwin";
        case "win32":
            return "serverPathWindows";
        default:
            return undefined;
    }
}

/** Expand `~`, `$VAR`, `${VAR}`, `${userHome}`, `${env:VAR}`, and `%VAR%`. */
function expandPath(value: string): string {
    let expanded = value.trim();
    if (expanded === "~") {
        expanded = os.homedir();
    } else if (expanded.startsWith("~/") || expanded.startsWith("~\\")) {
        expanded = path.join(os.homedir(), expanded.slice(2));
    }
    expanded = expanded
        .replace(/\$\{userHome\}/g, os.homedir())
        .replace(
            /\$\{env:([^}]+)\}/g,
            (_match, name: string) => process.env[name] ?? ""
        );
    return expanded.replace(
        /\$\{([^}]+)\}|\$([A-Za-z_][A-Za-z0-9_]*)|%([^%]+)%/g,
        (_match: string, braced?: string, bare?: string, percent?: string) => {
            const name = braced ?? bare ?? percent ?? "";
            return process.env[name] ?? "";
        }
    );
}

/** Resolve the server binary, preferring explicit settings over defaults. */
function resolveServerPath(
    config: vscode.WorkspaceConfiguration,
    context: vscode.ExtensionContext
): string {
    const explicit = expandPath(config.get<string>("serverPath", ""));
    if (explicit) {
        return explicit;
    }

    const osKey = osServerPathKey();
    if (osKey) {
        const osValue = expandPath(config.get<string>(osKey, ""));
        const osDefault = expandPath(
            config.inspect<string>(osKey)?.defaultValue ?? ""
        );
        // A user-set path wins even if it is wrong; a default is only used
        // when the binary is actually there.
        if (osValue && (osValue !== osDefault || fs.existsSync(osValue))) {
            return osValue;
        }
    }

    return defaultServerPath(context);
}

export function activate(context: vscode.ExtensionContext) {
    const config = vscode.workspace.getConfiguration("kvd");
    const serverPath = resolveServerPath(config, context);

    if (!fs.existsSync(serverPath)) {
        const osKey = osServerPathKey();
        const setting = osKey ? `kvd.${osKey}` : "kvd.serverPath";
        vscode.window.showErrorMessage(
            `kvd-lsp not found at ${serverPath}. Install it with \`cargo install kvd-lsp\`, or set ${setting}.`
        );
        return;
    }

    const serverOptions: ServerOptions = {
        args: [],
        command: serverPath,
        transport: TransportKind.stdio,
    };
    const clientOptions: LanguageClientOptions = {
        documentSelector: [{ language: "kvd", scheme: "file" }],
    };

    client = new LanguageClient("kvd", "KVD", serverOptions, clientOptions);
    client.start();
    context.subscriptions.push(
        vscode.commands.registerCommand("kvd.restartServer", async () => {
            await client?.stop();
            client?.start();
        })
    );
}

export function deactivate(): Thenable<void> | undefined {
    return client?.stop();
}
