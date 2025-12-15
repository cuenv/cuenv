import * as vscode from 'vscode';
import { EmbeddedRegion, Interpolation } from './types';

/**
 * Handles position translation between CUE source documents and virtual embedded documents.
 *
 * Since we replace CUE interpolations \(...) with equal-length placeholders,
 * position mapping is straightforward - we only need to adjust for the base offset.
 */
export class PositionMapper {
    /**
     * Convert a position in the CUE document to a position in the virtual document.
     * Returns null if the position is outside the embedded region.
     */
    toVirtualPosition(
        region: EmbeddedRegion,
        cueDocument: vscode.TextDocument,
        cuePosition: vscode.Position
    ): vscode.Position | null {
        const cueOffset = cueDocument.offsetAt(cuePosition);

        // Check if position is within the content region
        if (cueOffset < region.contentStartOffset || cueOffset > region.contentEndOffset) {
            return null;
        }

        // Calculate offset within the content
        const contentOffset = cueOffset - region.contentStartOffset;

        // Convert to line/character within virtual document
        const contentBefore = region.content.slice(0, contentOffset);
        const lines = contentBefore.split('\n');
        const line = lines.length - 1;
        const character = lines[lines.length - 1].length;

        return new vscode.Position(line, character);
    }

    /**
     * Convert a position in the virtual document back to a position in the CUE document.
     */
    fromVirtualPosition(
        region: EmbeddedRegion,
        cueDocument: vscode.TextDocument,
        virtualPosition: vscode.Position
    ): vscode.Position {
        // Calculate offset within content
        const lines = region.content.split('\n');
        let contentOffset = 0;

        for (let i = 0; i < virtualPosition.line && i < lines.length; i++) {
            contentOffset += lines[i].length + 1; // +1 for newline
        }
        contentOffset += Math.min(virtualPosition.character, lines[virtualPosition.line]?.length || 0);

        // Add base offset
        const cueOffset = region.contentStartOffset + contentOffset;

        return cueDocument.positionAt(cueOffset);
    }

    /**
     * Convert a range in the CUE document to a range in the virtual document.
     */
    toVirtualRange(
        region: EmbeddedRegion,
        cueDocument: vscode.TextDocument,
        cueRange: vscode.Range
    ): vscode.Range | null {
        const start = this.toVirtualPosition(region, cueDocument, cueRange.start);
        const end = this.toVirtualPosition(region, cueDocument, cueRange.end);

        if (!start || !end) return null;
        return new vscode.Range(start, end);
    }

    /**
     * Convert a range in the virtual document back to a range in the CUE document.
     */
    fromVirtualRange(
        region: EmbeddedRegion,
        cueDocument: vscode.TextDocument,
        virtualRange: vscode.Range
    ): vscode.Range {
        const start = this.fromVirtualPosition(region, cueDocument, virtualRange.start);
        const end = this.fromVirtualPosition(region, cueDocument, virtualRange.end);
        return new vscode.Range(start, end);
    }

    /**
     * Check if a CUE position is within a CUE interpolation.
     * Returns the interpolation if found, null otherwise.
     */
    getInterpolationAtOffset(region: EmbeddedRegion, cueOffset: number): Interpolation | null {
        return region.interpolations.find(
            interp => cueOffset >= interp.startOffset && cueOffset <= interp.endOffset
        ) || null;
    }

    /**
     * Check if a position in the virtual content corresponds to a placeholder.
     * Returns true if the position is within a replaced interpolation area.
     */
    isInPlaceholder(
        region: EmbeddedRegion,
        virtualOffset: number
    ): boolean {
        const cueOffset = region.contentStartOffset + virtualOffset;
        return this.getInterpolationAtOffset(region, cueOffset) !== null;
    }
}
