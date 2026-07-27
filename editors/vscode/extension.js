const vscode = require('vscode');
const child_process = require('child_process');
const path = require('path');
const fs = require('fs');

let lspProcess = null;
let diagnosticCollection = null;
let outputChannel = null;
let buffer = '';

function log(msg) {
    if (outputChannel) {
        outputChannel.appendLine(`[${new Date().toLocaleTimeString()}] ${msg}`);
    }
}

function activate(context) {
    outputChannel = vscode.window.createOutputChannel('Quince Language Server');
    context.subscriptions.push(outputChannel);
    log('Activating Quince Language Support extension...');

    diagnosticCollection = vscode.languages.createDiagnosticCollection('quince');
    context.subscriptions.push(diagnosticCollection);

    startServer(context);

    // Command to manually restart the LSP server
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.restartServer', () => {
            vscode.window.showInformationMessage('Restarting Quince LSP Server...');
            log('Manual server restart requested');
            restartServer(context);
        })
    );

    // Auto-reload watcher: Monitor target/debug/quince for binary rebuilds!
    const watcher = vscode.workspace.createFileSystemWatcher('**/target/debug/quince');
    watcher.onDidChange(() => {
        log('Detected target/debug/quince binary modification. Reloading LSP...');
        vscode.window.setStatusBarMessage('Quince binary updated. Reloading LSP...', 3000);
        restartServer(context);
    });
    context.subscriptions.push(watcher);

    // Document events
    context.subscriptions.push(
        vscode.workspace.onDidOpenTextDocument((doc) => {
            if (doc.languageId === 'quince') sendDidOpen(doc);
        })
    );

    context.subscriptions.push(
        vscode.workspace.onDidChangeTextDocument((event) => {
            if (event.document.languageId === 'quince') sendDidChange(event.document);
        })
    );

    context.subscriptions.push(
        vscode.workspace.onDidCloseTextDocument((doc) => {
            if (doc.languageId === 'quince') {
                diagnosticCollection.delete(doc.uri);
                sendDidClose(doc);
            }
        })
    );

    // Sync already open documents
    vscode.workspace.textDocuments.forEach((doc) => {
        if (doc.languageId === 'quince') sendDidOpen(doc);
    });
}

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

function startServer(context) {
    const binary = getBinaryPath();
    log(`Starting LSP binary at: ${binary}`);

    try {
        lspProcess = child_process.spawn(binary, ['lsp'], {
            stdio: ['pipe', 'pipe', 'inherit'],
        });
    } catch (err) {
        log(`Failed to launch process: ${err.message}`);
        vscode.window.showErrorMessage(`Failed to launch Quince LSP binary '${binary}': ${err.message}`);
        return;
    }

    lspProcess.on('error', (err) => {
        log(`Process error: ${err.message}`);
        vscode.window.showErrorMessage(`Quince LSP Error: ${err.message}`);
    });

    lspProcess.on('exit', (code) => {
        log(`LSP process exited with code ${code}`);
    });

    buffer = '';
    lspProcess.stdout.on('data', (data) => {
        buffer += data.toString('utf8');
        processBuffer();
    });

    // Send LSP initialize request
    sendJsonRpc({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
            processId: process.pid,
            rootUri: vscode.workspace.workspaceFolders?.[0]?.uri.toString() || null,
            capabilities: {},
        },
    });
}

function restartServer(context) {
    if (lspProcess) {
        try { lspProcess.kill(); } catch (e) {}
        lspProcess = null;
    }
    diagnosticCollection.clear();
    startServer(context);

    // Resync open documents
    vscode.workspace.textDocuments.forEach((doc) => {
        if (doc.languageId === 'quince') sendDidOpen(doc);
    });
}

function processBuffer() {
    while (true) {
        const headerEnd = buffer.indexOf('\r\n\r\n');
        if (headerEnd === -1) break;

        const header = buffer.substring(0, headerEnd);
        const match = header.match(/Content-Length:\s*(\d+)/i);
        if (!match) {
            buffer = buffer.substring(headerEnd + 4);
            continue;
        }

        const contentLength = parseInt(match[1], 10);
        const bodyStart = headerEnd + 4;

        if (buffer.length < bodyStart + contentLength) {
            break;
        }

        const bodyStr = buffer.substring(bodyStart, bodyStart + contentLength);
        buffer = buffer.substring(bodyStart + contentLength);

        try {
            const msg = JSON.parse(bodyStr);
            handleMessage(msg);
        } catch (e) {
            log(`JSON parse error: ${e.message}`);
        }
    }
}

function handleMessage(msg) {
    if (msg.method === 'textDocument/publishDiagnostics') {
        const uri = vscode.Uri.parse(msg.params.uri);
        log(`Received ${msg.params.diagnostics.length} diagnostics for ${uri.fsPath}`);
        const diagnostics = msg.params.diagnostics.map((d) => {
            const range = new vscode.Range(
                d.range.start.line,
                d.range.start.character,
                d.range.end.line,
                d.range.end.character
            );
            const diag = new vscode.Diagnostic(range, d.message, vscode.DiagnosticSeverity.Error);
            diag.source = 'quince';
            return diag;
        });
        diagnosticCollection.set(uri, diagnostics);
    }
}

function sendJsonRpc(obj) {
    if (!lspProcess || !lspProcess.stdin) return;
    const json = JSON.stringify(obj);
    const payload = `Content-Length: ${Buffer.byteLength(json, 'utf8')}\r\n\r\n${json}`;
    lspProcess.stdin.write(payload);
}

function sendDidOpen(doc) {
    log(`Sending textDocument/didOpen for ${doc.uri.fsPath}`);
    sendJsonRpc({
        jsonrpc: '2.0',
        method: 'textDocument/didOpen',
        params: {
            textDocument: {
                uri: doc.uri.toString(),
                languageId: 'quince',
                version: doc.version,
                text: doc.getText(),
            },
        },
    });
}

function sendDidChange(doc) {
    log(`Sending textDocument/didChange for ${doc.uri.fsPath}`);
    sendJsonRpc({
        jsonrpc: '2.0',
        method: 'textDocument/didChange',
        params: {
            textDocument: {
                uri: doc.uri.toString(),
                version: doc.version,
            },
            contentChanges: [
                {
                    text: doc.getText(),
                },
            ],
        },
    });
}

function sendDidClose(doc) {
    log(`Sending textDocument/didClose for ${doc.uri.fsPath}`);
    sendJsonRpc({
        jsonrpc: '2.0',
        method: 'textDocument/didClose',
        params: {
            textDocument: {
                uri: doc.uri.toString(),
            },
        },
    });
}

function deactivate() {
    if (lspProcess) {
        lspProcess.kill();
    }
}

module.exports = {
    activate,
    deactivate,
};
