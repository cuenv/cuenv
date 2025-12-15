import * as vscode from 'vscode';
import { EmbeddedLanguageDetector } from '../embedded/detector';
import { VirtualDocumentManager } from '../embedded/virtualDocument';
import { PositionMapper } from '../embedded/positionMapper';

/**
 * Provides code completions for embedded languages within CUE files.
 *
 * This provider intercepts completion requests in CUE files, detects if the
 * cursor is within an embedded code region, and forwards the request to the
 * appropriate language server via VS Code's executeCompletionItemProvider command.
 */
export class EmbeddedCompletionProvider implements vscode.CompletionItemProvider {
    constructor(
        private detector: EmbeddedLanguageDetector,
        private virtualDocManager: VirtualDocumentManager,
        private positionMapper: PositionMapper,
        private outputChannel?: vscode.OutputChannel
    ) {}

    private log(message: string) {
        this.outputChannel?.appendLine(`[Completion] ${message}`);
    }

    async provideCompletionItems(
        document: vscode.TextDocument,
        position: vscode.Position,
        token: vscode.CancellationToken,
        context: vscode.CompletionContext
    ): Promise<vscode.CompletionItem[] | vscode.CompletionList | null> {
        const offset = document.offsetAt(position);
        this.log(`Triggered at line ${position.line}, char ${position.character}, offset ${offset}`);

        // Find embedded region at cursor position
        const region = this.detector.findRegionAtPosition(document, position);
        if (!region) {
            // Log available regions for debugging
            const regions = this.detector.detectRegions(document);
            this.log(`Not in embedded region. Available regions: ${regions.map(r => `[${r.startOffset}-${r.endOffset}]`).join(', ')}`);
            return null; // Not in embedded region, let default provider handle
        }

        this.log(`In region: language=${region.language}, lines ${region.startLine}-${region.endLine}`);

        // Check if cursor is within a CUE interpolation
        const interpolation = this.positionMapper.getInterpolationAtOffset(region, offset);
        if (interpolation) {
            this.log('In CUE interpolation, skipping');
            return null; // In CUE interpolation, don't provide embedded completions
        }

        // Get regions to find index
        const regions = this.detector.detectRegions(document);
        const regionIndex = regions.indexOf(region);

        // Create virtual document URI
        const virtualUri = this.virtualDocManager.createVirtualUri(document.uri, region, regionIndex);
        this.log(`Virtual URI: ${virtualUri.toString()}`);

        // Map position to virtual document
        const virtualPosition = this.positionMapper.toVirtualPosition(region, document, position);
        if (!virtualPosition) {
            this.log('Failed to map position');
            return null;
        }

        this.log(`Virtual position: line ${virtualPosition.line}, char ${virtualPosition.character}`);

        try {
            // Ensure virtual document content is available
            const virtualDoc = await vscode.workspace.openTextDocument(virtualUri);
            this.log(`Virtual doc opened, languageId=${virtualDoc.languageId}, length=${virtualDoc.getText().length}`);

            // Forward to the appropriate language server
            const completions = await vscode.commands.executeCommand<vscode.CompletionList>(
                'vscode.executeCompletionItemProvider',
                virtualUri,
                virtualPosition,
                context.triggerCharacter
            );

            this.log(`Got ${completions?.items?.length ?? 0} completions`);

            if (!completions || !completions.items) {
                return null;
            }

            // Map completion item ranges back to CUE document
            const mappedItems = completions.items.map(item => {
                const mappedItem = new vscode.CompletionItem(item.label, item.kind);

                // Copy properties
                mappedItem.detail = item.detail;
                mappedItem.documentation = item.documentation;
                mappedItem.sortText = item.sortText;
                mappedItem.filterText = item.filterText;
                mappedItem.insertText = item.insertText;
                mappedItem.command = item.command;
                mappedItem.commitCharacters = item.commitCharacters;
                mappedItem.preselect = item.preselect;
                mappedItem.tags = item.tags;

                // Map text edit range if present
                if (item.range) {
                    if (item.range instanceof vscode.Range) {
                        mappedItem.range = this.positionMapper.fromVirtualRange(region, document, item.range);
                    } else {
                        // Handle {inserting, replacing} form
                        mappedItem.range = {
                            inserting: this.positionMapper.fromVirtualRange(region, document, item.range.inserting),
                            replacing: this.positionMapper.fromVirtualRange(region, document, item.range.replacing),
                        };
                    }
                }

                // Map additional text edits if present
                if (item.additionalTextEdits) {
                    mappedItem.additionalTextEdits = item.additionalTextEdits
                        .map(edit => {
                            const mappedRange = this.positionMapper.fromVirtualRange(region, document, edit.range);
                            return new vscode.TextEdit(mappedRange, edit.newText);
                        })
                        .filter(edit => {
                            // Only include edits within the embedded region
                            const startOffset = document.offsetAt(edit.range.start);
                            const endOffset = document.offsetAt(edit.range.end);
                            return startOffset >= region.contentStartOffset &&
                                   endOffset <= region.contentEndOffset;
                        });
                }

                return mappedItem;
            });

            return new vscode.CompletionList(mappedItems, completions.isIncomplete);
        } catch (error) {
            console.error('Embedded completion error:', error);
            return null;
        }
    }

    async resolveCompletionItem(
        item: vscode.CompletionItem,
        token: vscode.CancellationToken
    ): Promise<vscode.CompletionItem> {
        // Pass through - resolution happens on the original item
        return item;
    }
}
