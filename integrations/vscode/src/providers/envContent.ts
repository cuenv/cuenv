import * as vscode from 'vscode';
import { CuenvClient } from '../client';

export class EnvironmentContentProvider implements vscode.TextDocumentContentProvider {
    public static readonly scheme = 'cuenv-env';

    // Event emitter to signal when the content changes
    private _onDidChange = new vscode.EventEmitter<vscode.Uri>();
    readonly onDidChange = this._onDidChange.event;

    constructor(private client: CuenvClient) {}

    async provideTextDocumentContent(uri: vscode.Uri): Promise<string> {
        // URI query can contain the environment name
        // cuenv-env://authority/path?env=production
        const params = new URLSearchParams(uri.query);
        const envName = params.get('env');

        try {
            const envVars = await this.client.getEnvironmentVariables(envName || undefined);
            return JSON.stringify(envVars, null, 2);
        } catch (e) {
            return JSON.stringify({ error: String(e) }, null, 2);
        }
    }

    refresh(uri: vscode.Uri) {
        this._onDidChange.fire(uri);
    }
}
