import * as vscode from 'vscode';
import { EmbeddedRegion } from './types';
import { EmbeddedLanguageDetector } from './detector';

/**
 * URI scheme for virtual embedded documents.
 */
export const EMBEDDED_SCHEME = 'cuenv-embedded';

/**
 * Manages virtual documents for embedded code regions.
 *
 * Virtual documents allow VS Code's language servers to process
 * the embedded code as if it were a standalone file.
 */
export class VirtualDocumentManager implements vscode.TextDocumentContentProvider {
    private _onDidChange = new vscode.EventEmitter<vscode.Uri>();
    readonly onDidChange = this._onDidChange.event;

    private cache: Map<string, string> = new Map();

    constructor(private detector: EmbeddedLanguageDetector) {}

    /**
     * Generate a virtual document URI for an embedded region.
     */
    createVirtualUri(sourceUri: vscode.Uri, region: EmbeddedRegion, regionIndex: number): vscode.Uri {
        // Format: cuenv-embedded://typescript/encoded-source-path/region-0.ts
        const extension = this.getFileExtension(region.vscodeLanguageId);
        const encodedPath = encodeURIComponent(sourceUri.toString());

        return vscode.Uri.parse(
            `${EMBEDDED_SCHEME}://${region.vscodeLanguageId}/${encodedPath}/region-${regionIndex}${extension}`
        );
    }

    /**
     * Parse a virtual URI to extract source document info and region index.
     */
    parseVirtualUri(uri: vscode.Uri): { sourceUri: vscode.Uri; regionIndex: number; language: string } | null {
        if (uri.scheme !== EMBEDDED_SCHEME) return null;

        const language = uri.authority;
        const pathParts = uri.path.split('/');
        const encodedSourcePath = pathParts[1];
        const regionPart = pathParts[2]; // e.g., "region-0.ts"

        if (!encodedSourcePath || !regionPart) return null;

        const sourceUri = vscode.Uri.parse(decodeURIComponent(encodedSourcePath));
        const regionMatch = regionPart.match(/region-(\d+)/);
        const regionIndex = regionMatch ? parseInt(regionMatch[1], 10) : 0;

        return { sourceUri, regionIndex, language };
    }

    /**
     * TextDocumentContentProvider implementation.
     * Returns the content for a virtual document URI.
     */
    provideTextDocumentContent(uri: vscode.Uri): string | null {
        const cached = this.cache.get(uri.toString());
        if (cached !== undefined) {
            return cached;
        }

        const parsed = this.parseVirtualUri(uri);
        if (!parsed) return null;

        const sourceDocument = vscode.workspace.textDocuments.find(
            doc => doc.uri.toString() === parsed.sourceUri.toString()
        );
        if (!sourceDocument) return null;

        const regions = this.detector.detectRegions(sourceDocument);
        const region = regions[parsed.regionIndex];
        if (!region) return null;

        const virtualContent = this.createVirtualContent(region);
        this.cache.set(uri.toString(), virtualContent);

        return virtualContent;
    }

    /**
     * Create virtual document content by replacing interpolations with placeholders.
     */
    createVirtualContent(region: EmbeddedRegion): string {
        let content = region.content;

        // Sort interpolations by start offset (descending) to replace from end to start
        const sortedInterpolations = [...region.interpolations].sort(
            (a, b) => b.startOffset - a.startOffset
        );

        for (const interp of sortedInterpolations) {
            // Calculate relative offsets within content
            const relativeStart = interp.startOffset - region.contentStartOffset;
            const relativeEnd = interp.endOffset - region.contentStartOffset;
            const length = relativeEnd - relativeStart;

            // Replace with placeholder of same length
            // Use underscores for identifiers, wrapped in quotes if in string context
            const placeholder = '_'.repeat(length);

            content =
                content.slice(0, relativeStart) +
                placeholder +
                content.slice(relativeEnd);
        }

        return content;
    }

    /**
     * Notify that a virtual document has changed (when source document changes).
     */
    notifyChange(sourceUri: vscode.Uri): void {
        // Invalidate cache for all regions from this source
        const prefix = `${EMBEDDED_SCHEME}://`;
        const encodedSource = encodeURIComponent(sourceUri.toString());

        for (const key of this.cache.keys()) {
            if (key.includes(encodedSource)) {
                this.cache.delete(key);
                this._onDidChange.fire(vscode.Uri.parse(key));
            }
        }
    }

    /**
     * Clear all cached virtual documents.
     */
    clearCache(): void {
        this.cache.clear();
    }

    /**
     * Get file extension for a language ID.
     */
    private getFileExtension(languageId: string): string {
        const extensions: Record<string, string> = {
            typescript: '.ts',
            javascript: '.js',
            json: '.json',
            jsonc: '.jsonc',
            yaml: '.yaml',
            toml: '.toml',
            rust: '.rs',
            go: '.go',
            python: '.py',
            markdown: '.md',
            shellscript: '.sh',
            dockerfile: '',
            nix: '.nix',
            plaintext: '.txt',
        };
        return extensions[languageId] || '';
    }

    dispose(): void {
        this._onDidChange.dispose();
    }
}
