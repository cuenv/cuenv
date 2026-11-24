package rules

// #DirectoryRules defines directory-scoped configuration.
// Place in .rules.cue files throughout the repository.
// Each file is evaluated independently (no CUE unification).
#DirectoryRules: {
	// Ignore patterns for tool-specific ignore files
	// Generates files in the same directory as .rules.cue
	ignore?: #Ignore

	// Code ownership rules
	// Aggregated across all .rules.cue files to generate
	// a single CODEOWNERS file at the repository root
	owners?: #RulesOwners

	// EditorConfig settings
	// Generates .editorconfig in the same directory as .rules.cue
	editorconfig?: #EditorConfig

	// Directories, relative to this file, whose .rules.cue files
	// `cuenv sync -A` skips (for example test fixtures). Each entry is a
	// relative path inside this directory; nested .rules.cue files under
	// it are neither synced nor merged into CODEOWNERS.
	exclude?: [...string & !="" & !~"^/" & !~"(^|/)\\.{1,2}(/|$)"]
}
