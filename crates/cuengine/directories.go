package main

import (
	"io/fs"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// Reasons a directory is left out of a recursive evaluation. They mirror the
// rules of CUE's own "./..." walk (cue/load matchPackagesInFS).
const (
	SkippedReasonDot          = "dot"          // name starts with "."
	SkippedReasonUnderscore   = "underscore"   // name starts with "_"
	SkippedReasonTestdata     = "testdata"     // named "testdata"
	SkippedReasonNestedModule = "nestedModule" // holds its own cue.mod
	SkippedReasonUnreadable   = "unreadable"   // could not be listed
)

// Values accepted by ModuleEvalOptions.SkippedDirectories.
const (
	SkippedDirectoriesIgnore = "ignore"
	SkippedDirectoriesReport = "report"
)

// SkippedDirectory is a directory below the walk root that a recursive
// evaluation did not load although it holds CUE files.
type SkippedDirectory struct {
	// Path relative to the module root, slash-separated.
	Path string `json:"path"`
	// One of the SkippedReason constants.
	Reason string `json:"reason"`
}

// recursiveDirectories lists the directories a recursive ("./...") load
// visits below root, as "./"-relative load patterns ("." for root itself),
// and the directories it leaves out.
//
// CUE's own walk skips every directory whose name starts with "." or "_",
// including the walk root itself, so a module whose root directory is named
// ".config" or "_work" would load nothing. This walk applies the same rules
// to every directory except the root, which the caller asked for explicitly.
// The loader is then given the explicit directory list, so the result is the
// same as "./..." for every other root.
//
// A left-out directory is reported only when it holds at least one .cue
// file (at any depth), and only its topmost left-out ancestor is reported.
// Reporting walks into left-out trees, so it runs only when
// skippedDirectories is SkippedDirectoriesReport.
func recursiveDirectories(root string, moduleRoot string, skippedDirectories string) ([]string, []SkippedDirectory) {
	report := skippedDirectories == SkippedDirectoriesReport
	patterns := []string{}
	skipped := []SkippedDirectory{}
	moduleDirectory := filepath.Join(root, "cue.mod")

	_ = filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			// CUE ignores directories it cannot read; report them so a
			// caller that must see every instance can fail closed.
			if report && path != root {
				skipped = append(skipped, SkippedDirectory{Path: relativeSlashPath(moduleRoot, path), Reason: SkippedReasonUnreadable})
			}
			if entry != nil && entry.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		if !entry.IsDir() {
			return nil
		}
		if path == moduleDirectory {
			return filepath.SkipDir
		}
		if path != root {
			if reason := skippedReason(path, entry.Name()); reason != "" {
				if report && containsCueFile(path) {
					skipped = append(skipped, SkippedDirectory{Path: relativeSlashPath(moduleRoot, path), Reason: reason})
				}
				return filepath.SkipDir
			}
		}
		if !hasLoadableCueFile(path) {
			return nil
		}
		relative, relErr := filepath.Rel(root, path)
		if relErr != nil {
			return nil
		}
		if relative == "." {
			patterns = append(patterns, ".")
		} else {
			patterns = append(patterns, "./"+filepath.ToSlash(relative))
		}
		return nil
	})

	sort.Slice(skipped, func(i, j int) bool { return skipped[i].Path < skipped[j].Path })
	return patterns, skipped
}

// skippedReason says why CUE's walk leaves a (non-root) directory out, or ""
// when it is walked.
func skippedReason(path string, name string) string {
	switch {
	case strings.HasPrefix(name, "."):
		return SkippedReasonDot
	case strings.HasPrefix(name, "_"):
		return SkippedReasonUnderscore
	case name == "testdata":
		return SkippedReasonTestdata
	}
	if info, err := os.Stat(filepath.Join(path, "cue.mod")); err == nil && info != nil {
		return SkippedReasonNestedModule
	}
	return ""
}

// hasLoadableCueFile reports whether directory directly holds a .cue file
// the loader reads: CUE ignores file names starting with "." or "_". A
// directory without one is not a package directory; CUE's walk passes over
// it, while naming it explicitly would be an error.
func hasLoadableCueFile(directory string) bool {
	entries, err := os.ReadDir(directory)
	if err != nil {
		// Let the loader report the unreadable directory.
		return true
	}
	for _, entry := range entries {
		name := entry.Name()
		if entry.IsDir() || filepath.Ext(name) != ".cue" {
			continue
		}
		if strings.HasPrefix(name, ".") || strings.HasPrefix(name, "_") {
			continue
		}
		return true
	}
	return false
}

// containsCueFile reports whether any regular file below directory has the
// .cue extension. Unreadable subdirectories count as possibly holding one.
func containsCueFile(directory string) bool {
	found := false
	_ = filepath.WalkDir(directory, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			found = true
			return filepath.SkipAll
		}
		if !entry.IsDir() && filepath.Ext(entry.Name()) == ".cue" {
			found = true
			return filepath.SkipAll
		}
		return nil
	})
	return found
}

// relativeSlashPath names directory relative to moduleRoot with forward
// slashes, falling back to the absolute path outside the root.
func relativeSlashPath(moduleRoot string, directory string) string {
	return filepath.ToSlash(relativeInstancePath(moduleRoot, directory))
}
