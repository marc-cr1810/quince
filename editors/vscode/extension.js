const vscode = require('vscode');
const path = require('path');
const fs = require('fs');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

let client = null;
let statusBarItem = null;

function getBinaryName() {
    return process.platform === 'win32' ? 'quince.exe' : 'quince';
}

function getBinaryPath() {
    const execPath = vscode.workspace.getConfiguration('quince').get('executablePath');
    if (execPath && fs.existsSync(execPath)) {
        return execPath;
    }

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

    // 2. Search upwards from active text document directory
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

    // 3. Search PATH
    const onPath = findOnPath(binaryName);
    if (onPath) return onPath;

    // 4. Default fallback
    if (workspaceFolders && workspaceFolders.length > 0) {
        return path.join(workspaceFolders[0].uri.fsPath, 'target', 'debug', binaryName);
    }

    return binaryName;
}

function findOnPath(name) {
    const entries = (process.env.PATH || '').split(path.delimiter).filter(Boolean);
    for (const entry of entries) {
        const candidate = path.join(entry, name);
        try {
            if (fs.statSync(candidate).isFile()) return candidate;
        } catch {
            // keep looking
        }
    }
    return null;
}

function updateStatusBar(status, tooltip) {
    if (!statusBarItem) return;
    switch (status) {
        case 'ready':
            statusBarItem.text = '$(check) Quince';
            statusBarItem.tooltip = tooltip || 'Quince Language Server is active';
            break;
        case 'starting':
            statusBarItem.text = '$(sync~spin) Quince';
            statusBarItem.tooltip = 'Starting Quince Language Server...';
            break;
        case 'error':
            statusBarItem.text = '$(error) Quince';
            statusBarItem.tooltip = tooltip || 'Quince Language Server failed to start';
            break;
    }
    statusBarItem.show();
}

function getOrCreateTerminal(name) {
    const existing = vscode.window.terminals.find(t => t.name === name);
    if (existing) {
        return existing;
    }
    return vscode.window.createTerminal(name);
}

function runInTerminal(title, mainCommand) {
    const terminal = getOrCreateTerminal(title);
    terminal.show(true);
    terminal.sendText(mainCommand);
}

function startLspServer(command) {
    updateStatusBar('starting');
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

    client.onDidChangeState(event => {
        if (event.newState === 2 /* Running */) {
            updateStatusBar('ready', `Quince LSP running (${command})`);
        } else if (event.newState === 1 /* Stopped */) {
            updateStatusBar('error', 'Quince LSP server stopped');
        }
    });

    return client.start();
}

function activate(context) {
    const binaryPattern = `**/target/{debug,release}/${getBinaryName()}`;

    // Create status bar item
    statusBarItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
    statusBarItem.command = 'quince.showMenu';
    context.subscriptions.push(statusBarItem);

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

    tryStartClient().then(started => {
        if (!started) {
            updateStatusBar('error', 'Quince binary not found');
            vscode.window.showWarningMessage(
                "Quince language server not found. Install it with `cargo install --path .` " +
                "from a checkout, or run `cargo build` if working on Quince itself."
            );
        }
    }).catch(err => {
        updateStatusBar('error', err.message);
        console.error('Failed to start Quince LSP:', err);
    });

    // Menu / Quick Pick Command
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.showMenu', async () => {
            const pick = await vscode.window.showQuickPick([
                { label: '$(play) Run Current File', command: 'quince.runFile' },
                { label: '$(terminal) Open REPL', command: 'quince.openRepl' },
                { label: '$(folder-plus) Initialize New Project', command: 'quince.init' },
                { label: '$(beaker) Run Workspace Tests', command: 'quince.runTests' },
                { label: '$(refresh) Restart Language Server', command: 'quince.restartServer' },
            ], { placeHolder: 'Select a Quince tool or command' });

            if (pick && pick.command) {
                vscode.commands.executeCommand(pick.command);
            }
        })
    );

    // Command: Initialize New Project
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.init', async () => {
            const command = getBinaryPath();
            let targetDir = null;
            if (vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders.length > 0) {
                targetDir = vscode.workspace.workspaceFolders[0].uri.fsPath;
            }
            if (!targetDir) {
                const folderUri = await vscode.window.showOpenDialog({
                    canSelectFiles: false,
                    canSelectFolders: true,
                    canSelectMany: false,
                    openLabel: 'Select directory to initialize Quince project'
                });
                if (folderUri && folderUri.length > 0) {
                    targetDir = folderUri[0].fsPath;
                }
            }
            if (!targetDir) {
                return;
            }
            runInTerminal('Quince Terminal', `"${command}" init "${targetDir}"`);
            const mainFile = path.join(targetDir, 'main.qn');
            setTimeout(async () => {
                if (fs.existsSync(mainFile)) {
                    const doc = await vscode.workspace.openTextDocument(mainFile);
                    await vscode.window.showTextDocument(doc);
                }
            }, 800);
        })
    );

    // Command: Run Current File
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.runFile', () => {
            const editor = vscode.window.activeTextEditor;
            if (!editor) {
                vscode.window.showWarningMessage('No active Quince file to run.');
                return;
            }
            const filePath = editor.document.uri.fsPath;
            if (!filePath.endsWith('.qn')) {
                vscode.window.showWarningMessage('Active file is not a .qn source file.');
                return;
            }
            const command = getBinaryPath();
            runInTerminal('Quince Terminal', `"${command}" "${filePath}"`);
        })
    );

    // Command: Open REPL
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.openRepl', () => {
            const command = getBinaryPath();
            runInTerminal('Quince REPL', `"${command}"`);
        })
    );

    // Command: Run Tests
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.runTests', () => {
            runInTerminal('Quince Tests', 'cargo test');
        })
    );

    // Command: Restart Language Server
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.restartServer', async () => {
            const command = getBinaryPath();
            if (!fs.existsSync(command)) {
                vscode.window.showErrorMessage(`Quince binary not found at '${command}'.`);
                updateStatusBar('error');
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

    // Watcher for binary changes
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

    // Test Explorer Controller Integration
    const testController = vscode.tests.createTestController('quinceTestController', 'Quince Tests');
    context.subscriptions.push(testController);

    const testItem = testController.createTestItem('quinceSuite', 'Quince Workspace Test Suite');
    testController.items.add(testItem);

    testController.createRunProfile('Run Quince Tests', vscode.TestRunProfileKind.Run, async (request, token) => {
        const run = testController.createTestRun(request);
        run.started(testItem);
        try {
            const terminal = getOrCreateTerminal('Quince Tests');
            terminal.show(true);
            terminal.sendText('cargo test');
            run.passed(testItem);
        } catch (err) {
            run.failed(testItem, new vscode.TestMessage(err.message));
        } finally {
            run.end();
        }
    }, true);

    // Task Provider Integration
    context.subscriptions.push(
        vscode.tasks.registerTaskProvider('quince', {
            provideTasks: () => {
                const command = getBinaryPath();
                const runTask = new vscode.Task(
                    { type: 'quince', task: 'run' },
                    vscode.TaskScope.Workspace,
                    'Run File',
                    'quince',
                    new vscode.ShellExecution(`"${command}" "${vscode.window.activeTextEditor?.document.uri.fsPath || ''}"`)
                );
                const testTask = new vscode.Task(
                    { type: 'quince', task: 'test' },
                    vscode.TaskScope.Workspace,
                    'Run Tests',
                    'quince',
                    new vscode.ShellExecution('cargo test')
                );
                return [runTask, testTask];
            },
            resolveTask: (_task) => undefined,
        })
    );
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
