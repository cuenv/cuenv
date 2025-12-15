import * as vscode from 'vscode';

/**
 * Represents a CUE interpolation expression \(...) within embedded content.
 */
export interface Interpolation {
    /** Start offset of \( in the original CUE document */
    startOffset: number;
    /** End offset of ) in the original CUE document */
    endOffset: number;
    /** The expression content inside \(...) */
    expression: string;
}

/**
 * Represents a region of embedded code within a CUE file.
 */
export interface EmbeddedRegion {
    /** The language identifier (e.g., "typescript", "json") */
    language: string;
    /** VS Code language ID (may differ, e.g., "shell" -> "shellscript") */
    vscodeLanguageId: string;
    /** Start offset of opening """ in the CUE document */
    startOffset: number;
    /** End offset of closing """ in the CUE document */
    endOffset: number;
    /** Start offset of actual content (after opening """) */
    contentStartOffset: number;
    /** End offset of actual content (before closing """) */
    contentEndOffset: number;
    /** The raw content between the triple quotes */
    content: string;
    /** CUE interpolations within this region */
    interpolations: Interpolation[];
    /** Start line number (0-indexed) */
    startLine: number;
    /** End line number (0-indexed) */
    endLine: number;
}

/**
 * Special cases where CUE/cuenv language identifiers differ from VS Code language IDs.
 * Most languages map directly (typescript -> typescript, json -> json, etc.)
 */
const LANGUAGE_ALIASES: Record<string, string> = {
    shell: 'shellscript',
    text: 'plaintext',
};

/**
 * Get VS Code language ID from a cuenv language identifier.
 * Most languages map directly; only special cases need aliasing.
 */
export function getVSCodeLanguageId(language: string): string {
    const lower = language.toLowerCase();
    return LANGUAGE_ALIASES[lower] || lower;
}

/**
 * Known code schema types that should trigger embedded language support.
 * Other schema types like #Cube, #Project, etc. are NOT code types.
 */
const CODE_SCHEMA_TYPES = new Set([
    'Code', 'TypeScript', 'JavaScript', 'JSON', 'JSONC', 'YAML', 'TOML',
    'Rust', 'Go', 'Python', 'Markdown', 'Shell', 'Dockerfile', 'Nix', 'SQL',
]);

/**
 * Check if a schema type is a code type that should have embedded language support.
 */
export function isCodeSchemaType(schemaType: string): boolean {
    return CODE_SCHEMA_TYPES.has(schemaType);
}

/**
 * Convert a schema type name to a language identifier.
 * e.g., "TypeScript" -> "typescript", "JSON" -> "json"
 * Returns null if not a code schema type.
 */
export function schemaTypeToLanguage(schemaType: string): string | null {
    if (!isCodeSchemaType(schemaType)) {
        return null;
    }
    // Special case: #Code is the base type, defaults to text
    if (schemaType === 'Code') {
        return 'text';
    }
    // Otherwise, just lowercase the schema type name
    return schemaType.toLowerCase();
}
