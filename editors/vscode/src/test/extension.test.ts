import * as assert from "assert";
import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";

const fixtures = path.resolve(__dirname, "..", "..", "test", "fixtures");

function labelOf(item: vscode.CompletionItem): string {
    return typeof item.label === "string" ? item.label : item.label.label;
}

async function openFixture(name: string): Promise<vscode.TextDocument> {
    const uri = vscode.Uri.file(path.join(fixtures, name));
    const doc = await vscode.workspace.openTextDocument(uri);
    await vscode.window.showTextDocument(doc);
    return doc;
}

async function waitFor<T>(
    check: () => T | undefined | Promise<T | undefined>,
    timeoutMs = 15000
): Promise<T> {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
        const value = await check();
        if (value !== undefined) {
            return value;
        }
        await new Promise((resolve) => setTimeout(resolve, 100));
    }
    throw new Error(`timed out after ${timeoutMs}ms`);
}

async function completions(
    doc: vscode.TextDocument,
    position: vscode.Position
): Promise<vscode.CompletionItem[]> {
    const result = await vscode.commands.executeCommand<
        vscode.CompletionList | vscode.CompletionItem[]
    >("vscode.executeCompletionItemProvider", doc.uri, position);
    return Array.isArray(result) ? result : (result?.items ?? []);
}

suite("KVD extension", () => {
    suiteSetup(async () => {
        const serverPath = process.env.KVD_LSP_PATH;
        assert.ok(serverPath, "KVD_LSP_PATH is not set");
        assert.ok(
            fs.existsSync(serverPath),
            `kvd-lsp not found at ${serverPath}`
        );
        await vscode.workspace
            .getConfiguration("kvd")
            .update(
                "serverPath",
                serverPath,
                vscode.ConfigurationTarget.Global
            );
    });

    suiteTeardown(async () => {
        await vscode.workspace
            .getConfiguration("kvd")
            .update("serverPath", undefined, vscode.ConfigurationTarget.Global);
    });

    test("activates and registers the language command", async () => {
        const extension = vscode.extensions.getExtension(
            "KeyValueDocument.kvd-vscode"
        );
        assert.ok(extension, "extension is not installed in the test host");

        const doc = await openFixture("app.kvd");
        assert.strictEqual(doc.languageId, "kvd");

        await waitFor(() => (extension.isActive ? true : undefined));
        const commands = await vscode.commands.getCommands(true);
        assert.ok(
            commands.includes("kvd.restartServer"),
            "kvd.restartServer is not registered"
        );
    });

    test("completes keys from the sibling schema", async () => {
        const doc = await openFixture("app.kvd");
        const items = await waitFor(async () => {
            const current = await completions(doc, new vscode.Position(0, 2));
            return current.length > 0 ? current : undefined;
        });
        const labels = items.map(labelOf);
        assert.ok(labels.includes("timeout"), `schema key missing: ${labels}`);
    });

    test("completes values in a value position", async () => {
        const doc = await openFixture("app.kvd");
        await waitFor(async () => {
            const current = await completions(doc, new vscode.Position(0, 6));
            return current.some((item) => labelOf(item) === "true")
                ? true
                : undefined;
        });
    });

    test("reports sibling schema violations", async () => {
        const doc = await openFixture("bad.kvd");
        const diagnostics = await waitFor(() => {
            const current = vscode.languages.getDiagnostics(doc.uri);
            return current.length > 0 ? current : undefined;
        });
        assert.ok(
            diagnostics.some(
                (diagnostic) =>
                    diagnostic.severity === vscode.DiagnosticSeverity.Error
            ),
            `expected an error diagnostic: ${diagnostics.map((d) => d.message)}`
        );
    });
});
