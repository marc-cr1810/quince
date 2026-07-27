const vscode = require('vscode');
const path = require('path');
const fs = require('fs');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

let client = null;

function getBinaryPath() {
    const configPath = vscode.workspace.getConfiguration('quince').get('lspPath');
    if (configPath && fs.existsSync(configPath)) {
        return configPath;
    }

    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (workspaceFolders && workspaceFolders.length > 0) {
        const targetPath = path.join(workspaceFolders[0].uri.fsPath, 'target', 'debug', 'quince');
        if (fs.existsSync(targetPath)) {
            return targetPath;
        }
    }
    return 'quince';
}

function activate(context) {
    const command = getBinaryPath();

    const serverOptions = {
        command,
        args: ['lsp'],
        transport: TransportKind.stdio,
    };

    const clientOptions = {
        documentSelector: [{ scheme: 'file', language: 'quince' }],
        synchronize: {
            fileEvents: vscode.workspace.createFileSystemWatcher('**/target/debug/quince'),
        },
    };

    client = new LanguageClient(
        'quinceLanguageServer',
        'Quince Language Server',
        serverOptions,
        clientOptions
    );

    client.start();

    // Command to manually restart the LSP server
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.restartServer', async () => {
            if (client) {
                await client.restart();
                vscode.window.showInformationMessage('Quince LSP Server restarted');
            }
        })
    );

    // Auto-reload watcher: Monitor target/debug/quince for binary rebuilds!
    const watcher = vscode.workspace.createFileSystemWatcher('**/target/debug/quince');
    watcher.onDidChange(async () => {
        if (client) {
            vscode.window.setStatusBarMessage('Quince binary updated. Reloading LSP...', 3000);
            await client.restart();
        }
    });
    context.subscriptions.push(watcher);
}

function deactivate() {
    if (!client) {
        return undefined;
    }
    return client.stop();
}

module.exports = {
    activate,
    deactivate,
};
