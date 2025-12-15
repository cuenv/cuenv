import * as vscode from 'vscode';
import { EmbeddedRegion, Interpolation, schemaTypeToLanguage, getVSCodeLanguageId } from './types';

/**
 * Detects embedded language regions within CUE documents.
 *
 * Detection priority:
 * 1. Schema type constraint (e.g., schema.#TypeScript)
 * 2. Explicit language field (language: "typescript")
 * 3. Private hint field (_language: "typescript")
 */
export class EmbeddedLanguageDetector {
    private cache: Map<string, { version: number; regions: EmbeddedRegion[] }> = new Map();

    /**
     * Detect all embedded language regions in a document.
     */
    detectRegions(document: vscode.TextDocument): EmbeddedRegion[] {
        const uri = document.uri.toString();
        const cached = this.cache.get(uri);

        if (cached && cached.version === document.version) {
            return cached.regions;
        }

        const text = document.getText();
        const regions = this.parseDocument(text, document);

        this.cache.set(uri, { version: document.version, regions });
        return regions;
    }

    /**
     * Find the embedded region containing a specific position.
     */
    findRegionAtPosition(document: vscode.TextDocument, position: vscode.Position): EmbeddedRegion | undefined {
        const regions = this.detectRegions(document);
        const offset = document.offsetAt(position);

        return regions.find(region =>
            offset >= region.contentStartOffset && offset <= region.contentEndOffset
        );
    }

    /**
     * Clear the cache for a specific document or all documents.
     */
    clearCache(uri?: string): void {
        if (uri) {
            this.cache.delete(uri);
        } else {
            this.cache.clear();
        }
    }

    private parseDocument(text: string, document: vscode.TextDocument): EmbeddedRegion[] {
        const regions: EmbeddedRegion[] = [];

        // Pattern to find schema type constraints like: schema.#TypeScript & { or code.#JSON & {
        // Also matches without &: schema.#TypeScript {
        const schemaPattern = /(?:schema|code)\.#(\w+)\s*&?\s*\{/g;

        let schemaMatch;
        while ((schemaMatch = schemaPattern.exec(text)) !== null) {
            const schemaType = schemaMatch[1];
            const language = schemaTypeToLanguage(schemaType);

            // Find the content field within this block
            const blockStart = schemaMatch.index + schemaMatch[0].length;
            const contentRegion = this.findContentField(text, blockStart, language, document);

            if (contentRegion) {
                regions.push(contentRegion);
            }
        }

        // Also look for standalone language hints with private fields
        // Pattern: _language: "typescript" followed by content: """..."""
        const hintPattern = /_language:\s*"(\w+)"/g;
        let hintMatch;
        while ((hintMatch = hintPattern.exec(text)) !== null) {
            const language = hintMatch[1].toLowerCase();
            const searchStart = Math.max(0, hintMatch.index - 500); // Look back
            const searchEnd = Math.min(text.length, hintMatch.index + 500); // Look forward

            // Check if we already have a region covering this area
            const hintOffset = hintMatch.index;
            const alreadyCovered = regions.some(r =>
                hintOffset >= r.startOffset - 100 && hintOffset <= r.endOffset + 100
            );

            if (!alreadyCovered) {
                const contentRegion = this.findContentFieldNearby(text, hintMatch.index, language, document);
                if (contentRegion) {
                    regions.push(contentRegion);
                }
            }
        }

        // Look for explicit language field within code blocks
        // Pattern: language: "typescript" within a block that has content: """..."""
        const langFieldPattern = /language:\s*"(\w+)"/g;
        let langMatch;
        while ((langMatch = langFieldPattern.exec(text)) !== null) {
            const language = langMatch[1].toLowerCase();
            const langOffset = langMatch.index;

            // Check if already covered
            const alreadyCovered = regions.some(r =>
                langOffset >= r.startOffset - 200 && langOffset <= r.endOffset + 200
            );

            if (!alreadyCovered) {
                const contentRegion = this.findContentFieldNearby(text, langMatch.index, language, document);
                if (contentRegion) {
                    regions.push(contentRegion);
                }
            }
        }

        return regions;
    }

    private findContentField(
        text: string,
        searchStart: number,
        language: string,
        document: vscode.TextDocument
    ): EmbeddedRegion | null {
        // Look for content: """ within this block, tracking brace depth
        let depth = 1;
        let i = searchStart;

        while (i < text.length && depth > 0) {
            const char = text[i];

            if (char === '{') {
                depth++;
            } else if (char === '}') {
                depth--;
            } else if (char === 'c' && text.slice(i, i + 8) === 'content:') {
                // Found content field, look for """
                const tripleQuoteStart = text.indexOf('"""', i + 8);
                if (tripleQuoteStart !== -1 && tripleQuoteStart < i + 100) {
                    return this.extractRegion(text, tripleQuoteStart, language, document);
                }
            }
            i++;
        }

        return null;
    }

    private findContentFieldNearby(
        text: string,
        nearOffset: number,
        language: string,
        document: vscode.TextDocument
    ): EmbeddedRegion | null {
        // Search within 500 chars before and after
        const searchStart = Math.max(0, nearOffset - 500);
        const searchEnd = Math.min(text.length, nearOffset + 500);
        const searchText = text.slice(searchStart, searchEnd);

        const contentMatch = /content:\s*"""/.exec(searchText);
        if (contentMatch) {
            const absoluteOffset = searchStart + contentMatch.index;
            const tripleQuoteStart = text.indexOf('"""', absoluteOffset);
            if (tripleQuoteStart !== -1) {
                return this.extractRegion(text, tripleQuoteStart, language, document);
            }
        }

        return null;
    }

    private extractRegion(
        text: string,
        tripleQuoteStart: number,
        language: string,
        document: vscode.TextDocument
    ): EmbeddedRegion | null {
        const contentStart = tripleQuoteStart + 3; // After opening """

        // Find closing """
        const closingQuote = text.indexOf('"""', contentStart);
        if (closingQuote === -1) return null;

        const content = text.slice(contentStart, closingQuote);
        const interpolations = this.findInterpolations(content, contentStart);

        const startPos = document.positionAt(tripleQuoteStart);
        const endPos = document.positionAt(closingQuote + 3);

        return {
            language,
            vscodeLanguageId: getVSCodeLanguageId(language),
            startOffset: tripleQuoteStart,
            endOffset: closingQuote + 3,
            contentStartOffset: contentStart,
            contentEndOffset: closingQuote,
            content,
            interpolations,
            startLine: startPos.line,
            endLine: endPos.line,
        };
    }

    private findInterpolations(content: string, baseOffset: number): Interpolation[] {
        const interpolations: Interpolation[] = [];

        // CUE interpolation pattern: \(...)
        // Need to handle nested parentheses
        let i = 0;
        while (i < content.length - 1) {
            if (content[i] === '\\' && content[i + 1] === '(') {
                const start = i;
                let depth = 1;
                let j = i + 2;

                while (j < content.length && depth > 0) {
                    if (content[j] === '(') depth++;
                    else if (content[j] === ')') depth--;
                    j++;
                }

                if (depth === 0) {
                    interpolations.push({
                        startOffset: baseOffset + start,
                        endOffset: baseOffset + j,
                        expression: content.slice(start + 2, j - 1),
                    });
                    i = j;
                    continue;
                }
            }
            i++;
        }

        return interpolations;
    }
}
