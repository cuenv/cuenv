package main

import (
	iofs "io/fs"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing/fstest"

	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
)

// A snapshot holds the files its @embed attributes read, so an edit after freezing
// cannot change what it evaluates to. Which files those are is only known by running
// the attributes, as a glob or a file name may be anywhere in the instance's imports.
func captureEmbedded(root *build.Instance, instances []*build.Instance, sources map[string]bool) (map[string][]byte, error) {
	hasExtern := false
	for source := range sources {
		hasExtern = hasExtern || strings.Contains(source, "@extern")
	}
	if !hasExtern {
		return nil, nil
	}
	recorder := &embedRecorder{files: make(map[string][]byte)}
	for _, instance := range instances {
		instance.EmbedFS = recorder.at
	}
	// An error here is for whoever evaluates the snapshot: a file that could not be
	// read is not in it, so that evaluation reports the same error.
	cuecontext.New().BuildInstance(root)
	return recorder.files, nil
}

// embedFS is what @embed reads in dir when evaluating the snapshot.
func (graph frozenGraph) embedFS(dir string) iofs.StatFS {
	files := make(fstest.MapFS)
	for name, data := range graph.Embedded {
		relative, err := filepath.Rel(dir, name)
		if err == nil && filepath.IsLocal(relative) {
			files[filepath.ToSlash(relative)] = &fstest.MapFile{Data: data}
		}
	}
	return files
}

type embedRecorder struct {
	mutex sync.Mutex
	files map[string][]byte
}

func (recorder *embedRecorder) at(dir string) iofs.StatFS {
	return recordingFS{StatFS: os.DirFS(dir).(iofs.StatFS), dir: dir, recorder: recorder}
}

// recordingFS keeps the bytes of every file opened through it.
type recordingFS struct {
	iofs.StatFS
	dir      string
	recorder *embedRecorder
}

func (fsys recordingFS) Open(name string) (iofs.File, error) {
	info, err := fsys.StatFS.Stat(name)
	if err != nil || info.IsDir() {
		return fsys.StatFS.Open(name)
	}
	data, err := iofs.ReadFile(fsys.StatFS, name)
	if err != nil {
		return nil, err
	}
	fsys.recorder.mutex.Lock()
	fsys.recorder.files[filepath.Join(fsys.dir, filepath.FromSlash(name))] = data
	fsys.recorder.mutex.Unlock()
	return fstest.MapFS{name: {Data: data}}.Open(name)
}
