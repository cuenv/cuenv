import * as vscode from 'vscode';
import { EmbeddedLanguageDetector } from '../embedded/detector';
import { VirtualDocumentManager } from '../embedded/virtualDocument';
import { PositionMapper } from '../embedded/positionMapper';

/**
 * Provides hover information for embedded languages within CUE files.
 *
 * When hovering over code in an embedded region, this provider forwards
 * the request to the appropriate language server and returns the result.
 */
export class EmbeddedHoverProvider implements vscode.HoverProvider {
    constructor(
        private detector: EmbeddedLanguageDetector,
        private virtualDocManager: VirtualDocumentManager,
        private positionMapper: PositionMapper
    ) {}

    async provideHover(
        document: vscode.TextDocument,
        position: vscode.Position,
        token: vscode.CancellationToken
    ): Promise<vscode.Hover | null> {
        // Find embedded region at cursor position
        const region = this.detector.findRegionAtPosition(document, position);
        if (!region) {
            return null;
        }

        // Check if cursor is within a CUE interpolation
        const cueOffset = document.offsetAt(position);
        const interpolation = this.positionMapper.getInterpolationAtOffset(region, cueOffset);
        if (interpolation) {
            // Provide hover for CUE interpolation
            return new vscode.Hover(
                new vscode.MarkdownString(`**CUE Interpolation**\n\n\`\\(${interpolation.expression})\``),
                new vscode.Range(
                    document.positionAt(interpolation.startOffset),
                    document.positionAt(interpolation.endOffset)
                )
            );
        }

        // Get regions to find index
        const regions = this.detector.detectRegions(document);
        const regionIndex = regions.indexOf(region);

        // Create virtual document URI
        const virtualUri = this.virtualDocManager.createVirtualUri(document.uri, region, regionIndex);

        // Map position to virtual document
        const virtualPosition = this.positionMapper.toVirtualPosition(region, document, position);
        if (!virtualPosition) {
            return null;
        }

        try {
            // Ensure virtual document is available
            await vscode.workspace.openTextDocument(virtualUri);

            // Forward to the language server
            const hovers = await vscode.commands.executeCommand<vscode.Hover[]>(
                'vscode.executeHoverProvider',
                virtualUri,
                virtualPosition
            );

            if (!hovers || hovers.length === 0) {
                return null;
            }

            // Return the first hover, mapping the range back to CUE document
            const hover = hovers[0];

            if (hover.range) {
                const mappedRange = this.positionMapper.fromVirtualRange(region, document, hover.range);
                return new vscode.Hover(hover.contents, mappedRange);
            }

            return new vscode.Hover(hover.contents);
        } catch (error) {
            console.error('Embedded hover error:', error);
            return null;
        }
    }
}
