import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext) {
    const configured = vscode.workspace
        .getConfiguration("kvd")
        .get<string>("serverPath", "");
    const serverPath =
        configured ||
        path.join(
            context.extensionPath,
            "..",
            "..",
            "target",
            "debug",
            "kvd-lsp"
        );

    if (!fs.existsSync(serverPath)) {
        vscode.window.showErrorMessage(
            `kvd-lsp binary not found at ${serverPath}. Build it with \`cargo build\` in kvd-lsp/, or set kvd.serverPath.`
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
