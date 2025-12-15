import * as vscode from 'vscode';
import { EmbeddedLanguageDetector } from '../embedded/detector';
import { VirtualDocumentManager } from '../embedded/virtualDocument';
import { PositionMapper } from '../embedded/positionMapper';
import { EmbeddedRegion } from '../embedded/types';

/**
 * Standard semantic token types supported by VS Code.
 * These map to theme colors.
 */
const TOKEN_TYPES = [
    'namespace', 'type', 'class', 'enum', 'interface', 'struct',
    'typeParameter', 'parameter', 'variable', 'property', 'enumMember',
    'event', 'function', 'method', 'macro', 'keyword', 'modifier',
    'comment', 'string', 'number', 'regexp', 'operator', 'decorator'
];

/**
 * Standard semantic token modifiers.
 */
const TOKEN_MODIFIERS = [
    'declaration', 'definition', 'readonly', 'static', 'deprecated',
    'abstract', 'async', 'modification', 'documentation', 'defaultLibrary'
];

/**
 * The semantic tokens legend - defines what token types/modifiers we support.
 */
export const SEMANTIC_TOKENS_LEGEND = new vscode.SemanticTokensLegend(
    TOKEN_TYPES,
    TOKEN_MODIFIERS
);

/**
 * Provides semantic tokens for embedded code regions in CUE files.
 *
 * This provider:
 * 1. Detects embedded language regions in CUE files
 * 2. Creates virtual documents for each region
 * 3. Requests semantic tokens from VS Code's language servers
 * 4. Maps the tokens back to the original CUE file positions
 */
export class EmbeddedSemanticTokensProvider implements vscode.DocumentSemanticTokensProvider {
    constructor(
        private detector: EmbeddedLanguageDetector,
        private virtualDocManager: VirtualDocumentManager,
        private positionMapper: PositionMapper,
        private outputChannel?: vscode.OutputChannel
    ) {}

    private log(message: string) {
        this.outputChannel?.appendLine(`[SemanticTokens] ${message}`);
    }

    async provideDocumentSemanticTokens(
        document: vscode.TextDocument,
        token: vscode.CancellationToken
    ): Promise<vscode.SemanticTokens | null> {
        const regions = this.detector.detectRegions(document);

        if (regions.length === 0) {
            this.log('No embedded regions found');
            return null;
        }

        this.log(`Processing ${regions.length} embedded regions`);

        const builder = new vscode.SemanticTokensBuilder(SEMANTIC_TOKENS_LEGEND);

        for (let i = 0; i < regions.length; i++) {
            if (token.isCancellationRequested) break;

            const region = regions[i];
            await this.addTokensForRegion(document, region, i, builder);
        }

        return builder.build();
    }

    private async addTokensForRegion(
        document: vscode.TextDocument,
        region: EmbeddedRegion,
        regionIndex: number,
        builder: vscode.SemanticTokensBuilder
    ): Promise<void> {
        const virtualUri = this.virtualDocManager.createVirtualUri(document.uri, region, regionIndex);

        this.log(`Getting tokens for region ${regionIndex}: ${region.language} at lines ${region.startLine}-${region.endLine}`);

        try {
            // Open the virtual document to ensure content is available
            const virtualDoc = await vscode.workspace.openTextDocument(virtualUri);
            this.log(`Virtual doc languageId: ${virtualDoc.languageId}, uri: ${virtualDoc.uri.toString()}`);

            // Try multiple approaches to get semantic tokens
            let tokens: vscode.SemanticTokens | undefined;

            // Approach 1: Use the execute command
            try {
                const result = await vscode.commands.executeCommand<vscode.SemanticTokens>(
                    'vscode.provideDocumentSemanticTokens',
                    virtualUri
                );
                if (result && result.data && result.data.length > 0) {
                    tokens = result;
                    this.log(`Got tokens via provideDocumentSemanticTokens`);
                }
            } catch (e) {
                this.log(`provideDocumentSemanticTokens failed: ${e}`);
            }

            // Approach 2: Try executeDocumentSemanticTokensProvider (different command name)
            if (!tokens) {
                try {
                    const result = await vscode.commands.executeCommand<vscode.SemanticTokens>(
                        'vscode.executeDocumentSemanticTokensProvider',
                        virtualUri
                    );
                    if (result && result.data && result.data.length > 0) {
                        tokens = result;
                        this.log(`Got tokens via executeDocumentSemanticTokensProvider`);
                    }
                } catch (e) {
                    this.log(`executeDocumentSemanticTokensProvider failed: ${e}`);
                }
            }

            if (!tokens || !tokens.data || tokens.data.length === 0) {
                this.log(`No semantic tokens returned for region ${regionIndex} (tried both commands)`);
                return;
            }

            this.log(`Got ${tokens.data.length / 5} tokens for region ${regionIndex}`);

            // Decode and map tokens back to CUE document
            this.mapTokensToDocument(document, region, tokens, builder);

        } catch (error) {
            this.log(`Error getting tokens for region ${regionIndex}: ${error}`);
        }
    }

    private mapTokensToDocument(
        document: vscode.TextDocument,
        region: EmbeddedRegion,
        tokens: vscode.SemanticTokens,
        builder: vscode.SemanticTokensBuilder
    ): void {
        const data = tokens.data;

        // Semantic tokens are encoded as: [deltaLine, deltaStartChar, length, tokenType, tokenModifiers]
        // Each group of 5 integers represents one token
        let currentLine = 0;
        let currentChar = 0;

        for (let i = 0; i < data.length; i += 5) {
            const deltaLine = data[i];
            const deltaStartChar = data[i + 1];
            const length = data[i + 2];
            const tokenType = data[i + 3];
            const tokenModifiers = data[i + 4];

            // Calculate absolute position in virtual document
            if (deltaLine > 0) {
                currentLine += deltaLine;
                currentChar = deltaStartChar;
            } else {
                currentChar += deltaStartChar;
            }

            // Convert virtual position to CUE document position
            const virtualPos = new vscode.Position(currentLine, currentChar);
            const cuePos = this.positionMapper.fromVirtualPosition(region, document, virtualPos);

            // Check if this token is within an interpolation (skip if so)
            const cueOffset = document.offsetAt(cuePos);
            const interpolation = this.positionMapper.getInterpolationAtOffset(region, cueOffset);
            if (interpolation) {
                continue; // Skip tokens inside CUE interpolations
            }

            // Add token to builder
            // Note: builder.push expects absolute positions
            try {
                builder.push(
                    cuePos.line,
                    cuePos.character,
                    length,
                    tokenType,
                    tokenModifiers
                );
            } catch (e) {
                // Token might be out of order, which can happen with region mapping
                this.log(`Failed to push token at line ${cuePos.line}: ${e}`);
            }
        }
    }
}
