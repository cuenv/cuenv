import * as vscode from 'vscode';
import { EmbeddedLanguageDetector } from '../embedded/detector';
import { VirtualDocumentManager } from '../embedded/virtualDocument';
import { PositionMapper } from '../embedded/positionMapper';

/**
 * Provides go-to-definition for embedded languages within CUE files.
 *
 * When the user requests to go to definition on code in an embedded region,
 * this provider forwards the request to the appropriate language server.
 */
export class EmbeddedDefinitionProvider implements vscode.DefinitionProvider {
    constructor(
        private detector: EmbeddedLanguageDetector,
        private virtualDocManager: VirtualDocumentManager,
        private positionMapper: PositionMapper
    ) {}

    async provideDefinition(
        document: vscode.TextDocument,
        position: vscode.Position,
        token: vscode.CancellationToken
    ): Promise<vscode.Definition | vscode.LocationLink[] | null> {
        // Find embedded region at cursor position
        const region = this.detector.findRegionAtPosition(document, position);
        if (!region) {
            return null;
        }

        // Check if cursor is within a CUE interpolation
        const cueOffset = document.offsetAt(position);
        const interpolation = this.positionMapper.getInterpolationAtOffset(region, cueOffset);
        if (interpolation) {
            // Don't provide definition for CUE interpolations
            // A CUE language server should handle those
            return null;
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
            const definitions = await vscode.commands.executeCommand<vscode.Location[] | vscode.LocationLink[]>(
                'vscode.executeDefinitionProvider',
                virtualUri,
                virtualPosition
            );

            if (!definitions || definitions.length === 0) {
                return null;
            }

            // Map locations back to CUE document if they're in the virtual document
            const mappedLocations: vscode.Location[] = [];
            for (const def of definitions) {
                if ('targetUri' in def) {
                    // LocationLink - convert to Location
                    const link = def as vscode.LocationLink;
                    if (link.targetUri.toString() === virtualUri.toString()) {
                        mappedLocations.push(new vscode.Location(
                            document.uri,
                            this.positionMapper.fromVirtualRange(region, document, link.targetRange)
                        ));
                    } else {
                        mappedLocations.push(new vscode.Location(link.targetUri, link.targetRange));
                    }
                } else {
                    // Location
                    const loc = def as vscode.Location;
                    if (loc.uri.toString() === virtualUri.toString()) {
                        mappedLocations.push(new vscode.Location(
                            document.uri,
                            this.positionMapper.fromVirtualRange(region, document, loc.range)
                        ));
                    } else {
                        mappedLocations.push(loc);
                    }
                }
            }
            return mappedLocations;
        } catch (error) {
            console.error('Embedded definition error:', error);
            return null;
        }
    }
}
