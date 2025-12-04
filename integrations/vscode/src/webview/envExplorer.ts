import * as vscode from 'vscode';
import { CuenvClient } from '../client';

export class EnvWebview {
    public static readonly viewType = 'cuenv.envExplorer';

    public static show(extensionUri: vscode.Uri, client: CuenvClient) {
        const column = vscode.window.activeTextEditor
            ? vscode.window.activeTextEditor.viewColumn
            : undefined;

        // If we already have a panel, show it.
        if (EnvWebview.currentPanel) {
            EnvWebview.currentPanel._panel.reveal(column);
            return;
        }

        const panel = vscode.window.createWebviewPanel(
            EnvWebview.viewType,
            'Cuenv Environment Explorer',
            column || vscode.ViewColumn.One,
            {
                enableScripts: true,
                localResourceRoots: [extensionUri]
            }
        );

        EnvWebview.currentPanel = new EnvWebview(panel, client);
    }

    private static currentPanel: EnvWebview | undefined;

    private constructor(private readonly _panel: vscode.WebviewPanel, private readonly _client: CuenvClient) {
        this._update();

        this._panel.onDidDispose(() => this.dispose(), null, []);

        this._panel.webview.onDidReceiveMessage(
            async message => {
                switch (message.command) {
                    case 'getEnvironments':
                        const envs = await this._client.getEnvironments();
                        this._panel.webview.postMessage({ command: 'setEnvironments', environments: envs, current: this._client.getCurrentEnvironment() || 'Base' });
                        break;
                    case 'getVariables':
                        const vars = await this._client.getEnvironmentVariables(message.env);
                        this._panel.webview.postMessage({ command: 'setVariables', variables: vars });
                        break;
                }
            },
            null,
            []
        );
    }

    public dispose() {
        EnvWebview.currentPanel = undefined;
        this._panel.dispose();
    }

    private _update() {
        this._panel.webview.html = this._getHtmlForWebview();
    }

    private _getHtmlForWebview() {
        return `<!DOCTYPE html>
        <html lang="en">
        <head>
            <meta charset="UTF-8">
            <meta name="viewport" content="width=device-width, initial-scale=1.0">
            <title>Environment Explorer</title>
            <style>
                body { font-family: var(--vscode-font-family); padding: 20px; color: var(--vscode-foreground); background-color: var(--vscode-editor-background); }
                select { background: var(--vscode-dropdown-background); color: var(--vscode-dropdown-foreground); border: 1px solid var(--vscode-dropdown-border); padding: 5px; }
                table { width: 100%; border-collapse: collapse; margin-top: 20px; }
                th, td { text-align: left; padding: 8px; border-bottom: 1px solid var(--vscode-panel-border); }
                th { font-weight: bold; }
                .secret { color: var(--vscode-textPreformat-foreground); font-style: italic; }
                .value { font-family: var(--vscode-editor-font-family); }
            </style>
        </head>
        <body>
            <h2>Environment Explorer</h2>
            <div>
                <label for="env-select">Environment: </label>
                <select id="env-select"></select>
            </div>
            <table id="vars-table">
                <thead>
                    <tr>
                        <th>Variable</th>
                        <th>Value</th>
                    </tr>
                </thead>
                <tbody>
                </tbody>
            </table>
            <script>
                const vscode = acquireVsCodeApi();
                const select = document.getElementById('env-select');
                const tableBody = document.querySelector('#vars-table tbody');

                // Initial Load
                vscode.postMessage({ command: 'getEnvironments' });

                select.addEventListener('change', (e) => {
                    const env = e.target.value;
                    vscode.postMessage({ command: 'getVariables', env });
                });

                window.addEventListener('message', event => {
                    const message = event.data;
                    switch (message.command) {
                        case 'setEnvironments':
                            select.innerHTML = '';
                            message.environments.forEach(env => {
                                const option = document.createElement('option');
                                option.value = env;
                                option.textContent = env;
                                if (env === message.current) option.selected = true;
                                select.appendChild(option);
                            });
                            // Trigger load for initial selection
                            vscode.postMessage({ command: 'getVariables', env: select.value });
                            break;
                        case 'setVariables':
                            tableBody.innerHTML = '';
                            const vars = message.variables;
                            Object.keys(vars).sort().forEach(key => {
                                const row = document.createElement('tr');
                                const nameCell = document.createElement('td');
                                const valueCell = document.createElement('td');
                                
                                nameCell.textContent = key;
                                nameCell.className = 'value';
                                
                                const val = vars[key];
                                if (val === '[SECRET]') {
                                    valueCell.textContent = '******** (Secret)';
                                    valueCell.className = 'secret';
                                } else {
                                    valueCell.textContent = val;
                                    valueCell.className = 'value';
                                }
                                
                                row.appendChild(nameCell);
                                row.appendChild(valueCell);
                                tableBody.appendChild(row);
                            });
                            break;
                    }
                });
            </script>
        </body>
        </html>`;
    }
}
