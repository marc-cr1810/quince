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
                { label: '$(code) Inspect AST', command: 'quince.showAst' },
                { label: '$(list-tree) Inspect Tokens', command: 'quince.showTokens' },
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

    // Webview Inspector Panel
    let inspectorPanel = undefined;

    function showInspector(initialTab) {
        const editor = vscode.window.activeTextEditor;
        if (!editor) {
            vscode.window.showWarningMessage('No active Quince file to inspect.');
            return;
        }
        const filePath = editor.document.uri.fsPath;
        if (!filePath.endsWith('.qn')) {
            vscode.window.showWarningMessage('Active file is not a .qn source file.');
            return;
        }
        const fileName = path.basename(filePath);
        const command = getBinaryPath();
        const { execFile } = require('child_process');

        // Fetch AST and Tokens concurrently
        execFile(command, ['run', filePath, '--dump', 'ast'], (astErr, astStdout) => {
            execFile(command, ['run', filePath, '--dump', 'tokens'], (tokErr, tokStdout) => {
                const astContent = astStdout || astErr?.message || 'Failed to generate AST';
                const tokContent = tokStdout || tokErr?.message || 'Failed to generate Tokens';

                const column = vscode.ViewColumn.Beside;
                if (!inspectorPanel) {
                    inspectorPanel = vscode.window.createWebviewPanel(
                        'quinceInspector',
                        `Quince Inspector: ${fileName}`,
                        column,
                        { enableScripts: true }
                    );
                    inspectorPanel.onDidDispose(() => {
                        inspectorPanel = undefined;
                    }, null, context.subscriptions);

                    inspectorPanel.webview.onDidReceiveMessage(async message => {
                        if (message.command === 'jumpToSpan') {
                            try {
                                const doc = await vscode.workspace.openTextDocument(filePath);
                                const ed = await vscode.window.showTextDocument(doc, vscode.ViewColumn.One, false);
                                const startPos = doc.positionAt(message.start);
                                const endPos = doc.positionAt(message.end);
                                ed.selection = new vscode.Selection(startPos, endPos);
                                ed.revealRange(new vscode.Range(startPos, endPos), vscode.TextEditorRevealType.InCenter);
                            } catch (e) {
                                console.error('Error jumping to span:', e);
                            }
                        }
                    }, undefined, context.subscriptions);
                }

                inspectorPanel.title = `Quince Inspector: ${fileName}`;
                inspectorPanel.webview.html = getInspectorHtml(fileName, initialTab, astContent, tokContent);
            });
        });
    }

    function getInspectorHtml(fileName, initialTab, astContent, tokContent) {
        // Parse token lines into rows
        const tokenLines = tokContent.split('\n');
        let tokenRowsHtml = '';

        const keywords = new Set([
            'Class', 'Extends', 'Protected', 'Private', 'Public', 'Let', 'Fn', 'Op', 'Super',
            'If', 'Else', 'Return', 'Const', 'Complete', 'Sealed', 'Final', 'Import', 'From',
            'Extend', 'Try', 'Catch', 'Throw', 'In', 'SelfKw'
        ]);

        for (const rawLine of tokenLines) {
            const line = rawLine.trim();
            if (!line) continue;
            const match = line.match(/^(\d+\.\.\d+)\s+(.+)$/);
            if (!match) continue;
            const [, span, detail] = match;

            let category = 'Symbol';
            let badgeClass = 'badge-symbol';

            if (detail.startsWith('Ident(')) {
                category = 'Identifier';
                badgeClass = 'badge-ident';
            } else if (keywords.has(detail)) {
                category = 'Keyword';
                badgeClass = 'badge-keyword';
            } else if (detail.startsWith('Str(') || detail.startsWith('Int(') || detail.startsWith('Float(') || ['True', 'False', 'Nil'].includes(detail)) {
                category = 'Literal';
                badgeClass = 'badge-literal';
            } else if (['LParen', 'RParen', 'LBrace', 'RBrace', 'LBracket', 'RBracket', 'Colon', 'Comma', 'Semicolon'].includes(detail)) {
                category = 'Punctuation';
                badgeClass = 'badge-punct';
            } else {
                category = 'Operator';
                badgeClass = 'badge-operator';
            }

            const safeDetail = detail.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

            tokenRowsHtml += `
                <tr class="token-row clickable" data-category="${category.toLowerCase()}" data-detail="${safeDetail.toLowerCase()}" data-span="${span}" onclick="jumpToSpan('${span}')" title="Click to jump to source code">
                    <td><span class="span-tag">${span}</span></td>
                    <td><span class="badge ${badgeClass}">${category}</span></td>
                    <td class="token-value">${safeDetail}</td>
                </tr>
            `;
        }

        // Format AST into nested DOM blocks for clean code folding
        const astRawLines = astContent.split('\n');
        let stack = [];
        let htmlParts = [];

        for (let i = 0; i < astRawLines.length; i++) {
            const rawLine = astRawLines[i];
            if (!rawLine.trim() && htmlParts.length === 0) continue;

            const indent = Math.max(0, rawLine.search(/\S/));
            const escaped = rawLine.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
            let highlighted = escaped.replace(/("([^"\\]|\\.)*")|(\b[A-Z][A-Za-z0-9_]*\b)|(\b[a-z_][a-z0-9_]*:)|(\b\d+\b)/g, (match, pStr, _, pType, pKey, pNum) => {
                if (pStr) return `<span class="ast-string">${pStr}</span>`;
                if (pType) return `<span class="ast-type">${pType}</span>`;
                if (pKey) return `<span class="ast-key">${pKey.slice(0, -1)}</span><span class="ast-colon">:</span>`;
                if (pNum) return `<span class="ast-number">${pNum}</span>`;
                return match;
            });

            // Check if this line or next lines define a Span
            let spanBadgeHtml = '';
            if (rawLine.includes('Span {')) {
                const startMatch = astRawLines[i + 1]?.match(/start:\s*(\d+)/);
                const endMatch = astRawLines[i + 2]?.match(/end:\s*(\d+)/);
                if (startMatch && endMatch) {
                    const spanRange = `${startMatch[1]}..${endMatch[1]}`;
                    spanBadgeHtml = `<span class="span-jump-btn" onclick="event.stopPropagation(); jumpToSpan('${spanRange}')" title="Jump to code span ${spanRange}">📍 ${spanRange}</span>`;
                }
            }

            const trimmed = rawLine.trim();
            const rawTextLower = rawLine.toLowerCase().replace(/"/g, '&quot;');

            // Pop stack if current line indentation is less than or equal to current top block indent
            while (stack.length > 0 && stack[stack.length - 1] >= indent) {
                stack.pop();
                htmlParts.push('</div></div>');
            }

            const isOpening = trimmed.endsWith('{') || trimmed.endsWith('[') || trimmed.endsWith('(') || trimmed.includes('{') || trimmed.includes('[');

            if (isOpening) {
                stack.push(indent);
                htmlParts.push(`
                    <div class="ast-block" data-text="${rawTextLower}">
                        <div class="ast-header" onclick="toggleBlock(this)">
                            <span class="fold-icon">▼</span>
                            <span class="line-code">${highlighted}</span>
                            ${spanBadgeHtml}
                        </div>
                        <div class="ast-children">
                `);
            } else {
                htmlParts.push(`
                    <div class="ast-leaf" data-text="${rawTextLower}">
                        <span class="fold-spacer"></span>
                        <span class="line-code">${highlighted}</span>
                    </div>
                `);
            }
        }

        while (stack.length > 0) {
            stack.pop();
            htmlParts.push('</div></div>');
        }

        const astRowsHtml = htmlParts.join('');

        return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Quince Inspector</title>
<style>
    :root {
        --bg: #181825;
        --card: #1e1e2e;
        --border: #313244;
        --text: #cdd6f4;
        --subtext: #a6adc8;
        --accent: #89b4fa;
        --purple: #cba6f7;
        --green: #a6e3a1;
        --yellow: #f9e2af;
        --peach: #fab387;
        --line-num: #585b70;
    }
    body {
        background-color: var(--bg);
        color: var(--text);
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif;
        margin: 0;
        padding: 0;
        font-size: 13px;
    }
    .header {
        display: flex;
        align-items: center;
        justify-content: space-between;
        padding: 12px 20px;
        background: var(--card);
        border-bottom: 1px solid var(--border);
        position: sticky;
        top: 0;
        z-index: 100;
    }
    .title-group {
        display: flex;
        align-items: center;
        gap: 10px;
    }
    .title-group h2 {
        margin: 0;
        font-size: 15px;
        font-weight: 600;
        color: var(--text);
    }
    .file-badge {
        background: rgba(137, 180, 250, 0.15);
        color: var(--accent);
        padding: 3px 10px;
        border-radius: 12px;
        font-weight: 600;
        font-size: 12px;
        border: 1px solid rgba(137, 180, 250, 0.3);
    }
    .tabs {
        display: flex;
        gap: 8px;
    }
    .tab-btn {
        background: transparent;
        border: 1px solid var(--border);
        color: var(--subtext);
        padding: 6px 14px;
        border-radius: 6px;
        cursor: pointer;
        font-weight: 500;
        transition: all 0.2s ease;
    }
    .tab-btn:hover {
        background: var(--border);
        color: var(--text);
    }
    .tab-btn.active {
        background: var(--accent);
        color: var(--bg);
        border-color: var(--accent);
        font-weight: 600;
    }
    .filter-bar {
        padding: 12px 20px;
        background: var(--bg);
        border-bottom: 1px solid var(--border);
        display: flex;
        align-items: center;
        gap: 12px;
    }
    .filter-input {
        width: 100%;
        max-width: 340px;
        background: var(--card);
        border: 1px solid var(--border);
        color: var(--text);
        padding: 6px 12px;
        border-radius: 6px;
        outline: none;
        font-size: 12px;
    }
    .filter-input:focus {
        border-color: var(--accent);
    }
    .btn-small {
        background: var(--card);
        border: 1px solid var(--border);
        color: var(--subtext);
        padding: 5px 12px;
        border-radius: 6px;
        cursor: pointer;
        font-size: 12px;
        font-weight: 500;
        transition: all 0.2s ease;
    }
    .btn-small:hover {
        background: var(--border);
        color: var(--text);
    }
    .content-panel {
        display: none;
        padding: 20px;
    }
    .content-panel.active {
        display: block;
    }
    /* Tokens Table */
    .tokens-table {
        width: 100%;
        border-collapse: collapse;
        font-family: 'Cascadia Code', 'Fira Code', Consolas, Monaco, monospace;
        font-size: 12px;
    }
    .tokens-table th {
        text-align: left;
        padding: 10px 14px;
        color: var(--subtext);
        border-bottom: 2px solid var(--border);
        text-transform: uppercase;
        font-size: 11px;
        letter-spacing: 0.5px;
    }
    .tokens-table td {
        padding: 8px 14px;
        border-bottom: 1px solid var(--border);
    }
    .tokens-table tr:hover {
        background: rgba(255, 255, 255, 0.04);
    }
    .span-tag {
        color: var(--subtext);
        background: rgba(255, 255, 255, 0.06);
        padding: 2px 8px;
        border-radius: 4px;
        font-size: 11px;
    }
    .badge {
        display: inline-block;
        padding: 3px 8px;
        border-radius: 10px;
        font-size: 11px;
        font-weight: 600;
        letter-spacing: 0.3px;
    }
    .badge-keyword { background: rgba(203, 166, 247, 0.18); color: var(--purple); border: 1px solid rgba(203, 166, 247, 0.3); }
    .badge-ident { background: rgba(137, 180, 250, 0.18); color: var(--accent); border: 1px solid rgba(137, 180, 250, 0.3); }
    .badge-literal { background: rgba(166, 227, 161, 0.18); color: var(--green); border: 1px solid rgba(166, 227, 161, 0.3); }
    .badge-operator { background: rgba(249, 226, 175, 0.18); color: var(--yellow); border: 1px solid rgba(249, 226, 175, 0.3); }
    .badge-punct { background: rgba(250, 179, 135, 0.18); color: var(--peach); border: 1px solid rgba(250, 179, 135, 0.3); }
    .token-value {
        color: var(--text);
        font-weight: 500;
    }
    .token-row.clickable {
        cursor: pointer;
        transition: background 0.15s ease;
    }
    .token-row.clickable:hover {
        background: rgba(137, 180, 250, 0.12) !important;
    }
    .span-jump-btn {
        margin-left: 12px;
        background: rgba(137, 180, 250, 0.15);
        color: var(--accent);
        padding: 1px 8px;
        border-radius: 10px;
        font-size: 11px;
        cursor: pointer;
        border: 1px solid rgba(137, 180, 250, 0.3);
        transition: all 0.15s ease;
    }
    .span-jump-btn:hover {
        background: var(--accent);
        color: var(--bg);
    }
    /* AST Tree Formatting */
    .ast-container {
        background: var(--card);
        padding: 16px;
        border-radius: 8px;
        border: 1px solid var(--border);
        font-family: 'Cascadia Code', 'Fira Code', Consolas, Monaco, monospace;
        font-size: 12px;
        line-height: 1.6;
        overflow-x: auto;
    }
    .ast-block {
        margin-left: 0px;
    }
    .ast-header {
        display: flex;
        align-items: center;
        padding: 2px 6px;
        border-radius: 4px;
        cursor: pointer;
        user-select: none;
    }
    .ast-header:hover {
        background: rgba(255, 255, 255, 0.05);
    }
    .ast-leaf {
        display: flex;
        align-items: center;
        padding: 2px 6px;
        border-radius: 4px;
    }
    .ast-leaf:hover {
        background: rgba(255, 255, 255, 0.03);
    }
    .fold-icon {
        display: inline-block;
        width: 16px;
        font-size: 10px;
        color: var(--accent);
        cursor: pointer;
        user-select: none;
        flex-shrink: 0;
        margin-right: 4px;
        transition: transform 0.15s ease;
    }
    .fold-spacer {
        display: inline-block;
        width: 20px;
        flex-shrink: 0;
    }
    .ast-children {
        display: block;
    }
    .ast-block.collapsed > .ast-children {
        display: none !important;
    }
    .ast-block.collapsed > .ast-header .fold-icon {
        transform: rotate(-90deg);
    }
    .line-code {
        white-space: pre;
    }
    .ast-type { color: var(--purple); font-weight: 600; }
    .ast-key { color: var(--yellow); font-weight: 500; }
    .ast-colon { color: var(--subtext); }
    .ast-string { color: var(--green); }
    .ast-number { color: var(--peach); }
</style>
</head>
<body>
    <div class="header">
        <div class="title-group">
            <h2>Quince Inspector</h2>
            <span class="file-badge">${fileName}</span>
        </div>
        <div class="tabs">
            <button class="tab-btn ${initialTab === 'tokens' ? 'active' : ''}" onclick="switchTab('tokens')">🪙 Token Stream</button>
            <button class="tab-btn ${initialTab === 'ast' ? 'active' : ''}" onclick="switchTab('ast')">🌳 AST Tree</button>
        </div>
    </div>

    <div id="tokens-panel" class="content-panel ${initialTab === 'tokens' ? 'active' : ''}">
        <div class="filter-bar">
            <input type="text" id="token-filter" class="filter-input" placeholder="Filter tokens by span, category, or value..." oninput="filterTokens()">
        </div>
        <table class="tokens-table">
            <thead>
                <tr>
                    <th>Byte Span</th>
                    <th>Category</th>
                    <th>Token Details</th>
                </tr>
            </thead>
            <tbody id="token-rows">
                ${tokenRowsHtml}
            </tbody>
        </table>
    </div>

    <div id="ast-panel" class="content-panel ${initialTab === 'ast' ? 'active' : ''}">
        <div class="filter-bar">
            <input type="text" id="ast-filter" class="filter-input" placeholder="Filter AST nodes..." oninput="filterAst()">
            <button class="btn-small" onclick="expandAllAst()">Expand All</button>
            <button class="btn-small" onclick="collapseAllAst()">Collapse All</button>
        </div>
        <div class="ast-container" id="ast-tree-root">
            ${astRowsHtml}
        </div>
    </div>

    <script>
        const vscode = acquireVsCodeApi();

        function jumpToSpan(spanStr) {
            if (!spanStr) return;
            const parts = spanStr.split('..').map(n => parseInt(n, 10));
            if (parts.length === 2 && !isNaN(parts[0]) && !isNaN(parts[1])) {
                vscode.postMessage({ command: 'jumpToSpan', start: parts[0], end: parts[1] });
            }
        }
        function toggleBlock(headerEl) {
            const block = headerEl.closest('.ast-block');
            if (block) {
                block.classList.toggle('collapsed');
            }
        }

        function expandAllAst() {
            document.querySelectorAll('.ast-block').forEach(block => {
                block.classList.remove('collapsed');
            });
        }

        function collapseAllAst() {
            document.querySelectorAll('.ast-block').forEach(block => {
                block.classList.add('collapsed');
            });
        }

        function switchTab(tab) {
            document.querySelectorAll('.tab-btn').forEach(btn => btn.classList.remove('active'));
            document.querySelectorAll('.content-panel').forEach(panel => panel.classList.remove('active'));

            if (tab === 'tokens') {
                document.querySelectorAll('.tab-btn')[0].classList.add('active');
                document.getElementById('tokens-panel').classList.add('active');
            } else {
                document.querySelectorAll('.tab-btn')[1].classList.add('active');
                document.getElementById('ast-panel').classList.add('active');
            }
        }

        function filterTokens() {
            const query = document.getElementById('token-filter').value.toLowerCase();
            const rows = document.querySelectorAll('.token-row');
            rows.forEach(row => {
                const text = row.dataset.span + ' ' + row.dataset.category + ' ' + row.dataset.detail;
                row.style.display = text.includes(query) ? '' : 'none';
            });
        }

        function filterAst() {
            const query = document.getElementById('ast-filter').value.toLowerCase();
            const blocks = document.querySelectorAll('.ast-block, .ast-leaf');
            blocks.forEach(el => {
                const text = el.dataset.text || '';
                el.style.display = text.includes(query) ? '' : 'none';
            });
        }
    </script>
</body>
</html>`;
    }

    // Command: Inspect AST
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.showAst', () => showInspector('ast'))
    );

    // Command: Inspect Tokens
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.showTokens', () => showInspector('tokens'))
    );

    // Command: Run Current File (supports editor button and explorer context menu)
    context.subscriptions.push(
        vscode.commands.registerCommand('quince.runFile', (uri) => {
            let filePath = uri ? uri.fsPath : (vscode.window.activeTextEditor ? vscode.window.activeTextEditor.document.uri.fsPath : null);
            if (!filePath) {
                vscode.window.showWarningMessage('No Quince file selected to run.');
                return;
            }
            if (!filePath.endsWith('.qn')) {
                vscode.window.showWarningMessage('Selected file is not a .qn source file.');
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
