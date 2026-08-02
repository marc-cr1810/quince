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

    // 3. An installed Quince, found the way a shell finds one.
    //
    // This is what makes `cargo install quince` enough. Without it the only
    // supported layout was a checkout with a `target/` directory in it, and
    // anybody who installed the binary properly was told to run `cargo build`
    // — the fallback below returned the bare name, and `fs.existsSync` resolves
    // a bare name against the process working directory rather than PATH.
    const onPath = findOnPath(binaryName);
    if (onPath) return onPath;

    // 4. Nothing found. Name the place a checkout would put it, so the message
    // about it missing points somewhere the reader recognises.
    if (workspaceFolders && workspaceFolders.length > 0) {
        return path.join(workspaceFolders[0].uri.fsPath, 'target', 'debug', binaryName);
    }

    return binaryName;
}

/// Where `PATH` would find `name`, or null.
function findOnPath(name) {
    const entries = (process.env.PATH || '').split(path.delimiter).filter(Boolean);
    // Windows resolves a bare name against PATHEXT; the caller has already
    // appended `.exe`, which is the only extension a Rust build produces.
    for (const entry of entries) {
        const candidate = path.join(entry, name);
        try {
            if (fs.statSync(candidate).isFile()) return candidate;
        } catch {
            // Not there, or not readable. Either way, keep looking.
        }
    }
    return null;
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
                "Quince language server not found. Install it with `cargo install --path .` " +
                "from a checkout, or run `cargo build` if you are working on Quince itself. " +
                "A specific binary can be set with the `quince.lspPath` setting."
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
                vscode.window.showErrorMessage(
                `Quince binary not found at '${command}'. Install it with ` +
                '`cargo install --path .`, or run `cargo build` if you are working on Quince itself.'
            );
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


