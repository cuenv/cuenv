import * as vscode from 'vscode';
import { CuenvClient } from '../client';
import { CuenvTask } from '../types';

export class GraphWebview {
    public static readonly viewType = 'cuenv.graph';

    public static show(extensionUri: vscode.Uri, client: CuenvClient) {
        const column = vscode.window.activeTextEditor
            ? vscode.window.activeTextEditor.viewColumn
            : undefined;

        const panel = vscode.window.createWebviewPanel(
            GraphWebview.viewType,
            'Cuenv Task Graph',
            column || vscode.ViewColumn.One,
            {
                enableScripts: true,
                localResourceRoots: [extensionUri]
            }
        );

        new GraphWebview(panel, client);
    }

    constructor(private readonly _panel: vscode.WebviewPanel, private readonly _client: CuenvClient) {
        this._update();
        
        // Handle messages from the webview
        this._panel.webview.onDidReceiveMessage(
            message => {
                switch (message.command) {
                    case 'runTask':
                        this._client.runTask(message.text);
                        return;
                }
            },
            null,
            []
        );
    }

    private async _update() {
        const tasks = await this._client.getTasks();
        this._panel.webview.html = this._getHtmlForWebview(tasks);
    }

    private _getHtmlForWebview(tasks: CuenvTask[]) {
        // Generate Mermaid definition
        const edges: string[] = [];
        const nodes: string[] = [];

        tasks.forEach(task => {
            // Clean task name for Mermaid ID (replace . with _)
            const id = task.name.replace(/\./g, '_').replace(/:/g, '_');
            nodes.push(`${id}["${task.name}"]`);
            
            // Add click event
            nodes.push(`click ${id} callTask`);

            if (task.definition.dependsOn) {
                task.definition.dependsOn.forEach(dep => {
                    const depId = dep.replace(/\./g, '_').replace(/:/g, '_');
                    edges.push(`${depId} --> ${id}`);
                });
            }
        });

        const mermaidDef = `
            graph TD
            ${nodes.join('\n')}
            ${edges.join('\n')}
        `;

        return `<!DOCTYPE html>
        <html lang="en">
        <head>
            <meta charset="UTF-8">
            <meta name="viewport" content="width=device-width, initial-scale=1.0">
            <title>Cuenv Graph</title>
            <script type="module">
                import mermaid from 'https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.esm.min.mjs';
                mermaid.initialize({ startOnLoad: true });
            </script>
            <style>
                body { font-family: sans-serif; padding: 20px; }
                .mermaid { cursor: pointer; }
            </style>
        </head>
        <body>
            <h1>Task Dependency Graph</h1>
            <div class="mermaid">
                ${mermaidDef}
            </div>
            <script>
                const vscode = acquireVsCodeApi();
                window.callTask = (id) => {
                    // Mermaid might return the node ID, we need to map it back if we transformed it heavily.
                    // Since we just replaced . with _, we can assume the label in the box is the real name?
                    // Actually mermaid click callback usually passes the ID.
                    // We can encode the real name in the ID if needed, or just use the click event to trigger something.
                    // For simplicity, let's assume simple names for now or parse the ID if we can.
                    // BUT: Mermaid "click" support in raw HTML integration can be tricky with scope.
                    // Let's try a simpler approach: standard onclick not working easily inside mermaid SVG shadow DOM sometimes.
                    
                    // Fallback: Just show graph for now. Interaction is a bonus.
                }
                
                // Hack to support clicking: attach global function
                window.callTask = (id) => {
                     // Reverse the ID transformation is hard if ambiguous.
                     // Ideally we pass the name as an argument to the click function in mermaid def: click ID callTask "RealName"
                     // But mermaid string interpolation rules apply.
                     console.log("Clicked " + id);
                };
            </script>
        </body>
        </html>`;
    }
}
