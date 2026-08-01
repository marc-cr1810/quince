const vscode = require('vscode');
const path = require('path');
const fs = require('fs');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

let client = null;

function getBinaryName() {
    return process.platform === 'win32' ? 'quince.exe' : 'quince';
}

function getBinaryPath() {
    const configPath = vscode.workspace.getConfiguration('quince').get('lspPath');
    if (configPath && fs.existsSync(configPath)) {
        return configPath;
    }

    const binaryName = getBinaryName();

    // 1. Check workspace folders
    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (workspaceFolders && workspaceFolders.length > 0) {
        for (const folder of workspaceFolders) {
            const debugPath = path.join(folder.uri.fsPath, 'target', 'debug', binaryName);
            if (fs.existsSync(debugPath)) return debugPath;

            const releasePath = path.join(folder.uri.fsPath, 'target', 'release', binaryName);
            if (fs.existsSync(releasePath)) return releasePath;
        }
    }

    // 2. Search upwards from active text document directory (e.g. for examples/hello.qn)
    if (vscode.window.activeTextEditor) {
        let dir = path.dirname(vscode.window.activeTextEditor.document.uri.fsPath);
        while (dir && dir !== path.dirname(dir)) {
            const debugPath = path.join(dir, 'target', 'debug', binaryName);
            if (fs.existsSync(debugPath)) return debugPath;

            const releasePath = path.join(dir, 'target', 'release', binaryName);
            if (fs.existsSync(releasePath)) return releasePath;

            dir = path.dirname(dir);
        }
    }

    // 3. Fallback: return path in first workspace folder if available
    if (workspaceFolders && workspaceFolders.length > 0) {
        return path.join(workspaceFolders[0].uri.fsPath, 'target', 'debug', binaryName);
    }

    return binaryName;
}

function startLspServer(command) {
    const serverOptions = {
        command,
        args: ['lsp'],
        transport: TransportKind.stdio,
    };

    const binaryName = getBinaryName();
    const clientOptions = {
        documentSelector: [{ scheme: 'file', language: 'quince' }],
        synchronize: {
            fileEvents: vscode.workspace.createFileSystemWatcher(`**/target/{debug,release}/${binaryName}`),
        },
    };

    client = new LanguageClient(
        'quinceLanguageServer',
        'Quince Language Server',
        serverOptions,
        clientOptions
    );

    return client.start();
}

function activate(context) {
    const binaryPattern = `**/target/{debug,release}/${getBinaryName()}`;

    async function tryStartClient() {
        const command = getBinaryPath();
        if (fs.existsSync(command)) {
            if (!client) {
                await startLspServer(command);
            } else {
                await client.restart();
            }
            return true;
        }
        return false;
    }

    // Try starting on activation
    tryStartClient().then(started => {
        if (!started) {
            vscode.window.showWarningMessage(
                "Quince LSP binary not found. Run 'cargo build' or 'cargo build --release' in your workspace to enable Language Server features."
            );
        }
    }).catch(err => {
        console.error('Failed to start Quince LSP:', err);
    });

    // Command to manually restart/start the LSP server
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.restartServer', async () => {
            const command = getBinaryPath();
            if (!fs.existsSync(command)) {
                vscode.window.showErrorMessage(`Quince binary not found at '${command}'. Please run 'cargo build' or 'cargo build --release'.`);
                return;
            }
            if (client) {
                await client.restart();
                vscode.window.showInformationMessage('Quince LSP Server restarted');
            } else {
                await startLspServer(command);
                vscode.window.showInformationMessage('Quince LSP Server started');
            }
        })
    );

    // Auto-reload watcher: Monitor target/debug and target/release binaries for rebuilds or creations!
    const watcher = vscode.workspace.createFileSystemWatcher(binaryPattern);

    const onBinaryChanged = async () => {
        const command = getBinaryPath();
        if (fs.existsSync(command)) {
            if (client) {
                vscode.window.setStatusBarMessage('Quince binary updated. Reloading LSP...', 3000);
                await client.restart();
            } else {
                vscode.window.setStatusBarMessage('Quince binary detected. Starting LSP...', 3000);
                await startLspServer(command);
            }
        }
    };

    watcher.onDidCreate(onBinaryChanged);
    watcher.onDidChange(onBinaryChanged);
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


