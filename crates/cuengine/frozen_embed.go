package main

import (
	"io"
	iofs "io/fs"
	"path"
	"sync"
	"testing/fstest"

	"cuelang.org/go/cue/build"
	"cuelang.org/go/cue/cuecontext"
	"cuelang.org/go/cue/token"
)

type frozenLocation struct {
	Space string `json:"space"`
	Path  string `json:"path"`
}

func freezeLocation(loc token.FSLoc) *frozenLocation {
	if loc.IsZero() {
		return nil
	}
	// The loader gives every file in a filesystem the same mapping from
	// its root to a display path. OS volumes can have the same FS-relative
	// paths, while Config.FS can use names unrelated to host paths.
	space := "."
	if loc.FromFSPath != nil {
		space = loc.FromFSPath(space)
	}
	return &frozenLocation{Space: space, Path: loc.Path}
}

func (graph frozenGraph) restoreLocation(loc *frozenLocation, displayPath string) token.FSLoc {
	if loc == nil {
		return token.FSLoc{}
	}
	return token.FSLoc{
		FS:   graph.Embedded[loc.Space],
		Path: loc.Path,
		FromFSPath: func(name string) string {
			if name == loc.Path {
				return displayPath
			}
			return name
		},
	}
}

// captureEmbedded records the filesystem operations CUE's embed injector
// actually performs. Wrap the loader's FS on the source token, so overlays
// and the loader's virtual path namespace are observed by the injector.
func captureEmbedded(root *build.Instance, instances []*build.Instance) map[string]frozenEmbedFS {
	recorder := &embedRecorder{spaces: make(map[string]frozenEmbedFS)}
	original := make(map[*token.File]token.FSLoc)
	defer func() {
		for file, loc := range original {
			file.SetFSLoc(loc)
		}
	}()
	for _, instance := range instances {
		for _, syntax := range instance.Files {
			if len(syntax.Decls) == 0 {
				continue
			}
			file := syntax.Pos().File()
			if file == nil {
				continue
			}
			loc := file.FSLoc()
			if _, wrapped := original[file]; wrapped || loc.FS == nil {
				continue
			}
			original[file] = loc
			loc.FS = recordingFS{base: loc.FS, space: freezeLocation(loc).Space, prefix: ".", recorder: recorder}
			file.SetFSLoc(loc)
		}
	}
	// A failed source read is not recorded. The isolated replay still
	// fails the embed when evaluated.
	cuecontext.New(cuecontext.CompleteInternalReferences()).BuildInstance(root)
	return recorder.spaces
}

type embedRecorder struct {
	mutex  sync.Mutex
	spaces map[string]frozenEmbedFS
}

func (r *embedRecorder) record(space string, update func(frozenEmbedFS)) {
	r.mutex.Lock()
	defer r.mutex.Unlock()
	files, ok := r.spaces[space]
	if !ok {
		files = frozenEmbedFS{
			Files:       make(map[string][]byte),
			Stats:       make(map[string]iofs.FileMode),
			Directories: make(map[string][]frozenEmbedEntry),
		}
		r.spaces[space] = files
	}
	update(files)
}

// Directory entries and stat results are significant: the CUE
// injector's glob walker relies on both, and a nested cue.mod entry must
// keep a glob from embedding files belonging to another module.
type frozenEmbedFS struct {
	Files       map[string][]byte             `json:"files,omitempty"`
	Stats       map[string]iofs.FileMode      `json:"stats,omitempty"`
	Directories map[string][]frozenEmbedEntry `json:"directories,omitempty"`
}

type frozenEmbedEntry struct {
	Name string        `json:"name"`
	Mode iofs.FileMode `json:"mode"`
}

type recordingFS struct {
	base     iofs.FS
	space    string
	prefix   string
	recorder *embedRecorder
}

func (fsys recordingFS) Sub(dir string) (iofs.FS, error) {
	sub, err := iofs.Sub(fsys.base, dir)
	if err != nil {
		return nil, err
	}
	fsys.base = sub
	fsys.prefix = path.Join(fsys.prefix, dir)
	return fsys, nil
}

func (fsys recordingFS) Open(name string) (iofs.File, error) {
	file, err := fsys.base.Open(name)
	if err != nil {
		return nil, err
	}
	info, err := file.Stat()
	if err != nil {
		return file, nil
	}
	key := path.Join(fsys.prefix, name)
	if info.IsDir() {
		fsys.recorder.record(fsys.space, func(files frozenEmbedFS) {
			files.Stats[key] = info.Mode()
		})
		return file, nil
	}
	data, err := io.ReadAll(file)
	_ = file.Close()
	if err != nil {
		return nil, err
	}
	fsys.recorder.record(fsys.space, func(files frozenEmbedFS) {
		files.Files[key] = data
		files.Stats[key] = info.Mode()
	})
	return fstest.MapFS{name: {Data: data, Mode: info.Mode()}}.Open(name)
}

func (fsys recordingFS) Stat(name string) (iofs.FileInfo, error) {
	info, err := iofs.Stat(fsys.base, name)
	if err == nil {
		fsys.recorder.record(fsys.space, func(files frozenEmbedFS) {
			files.Stats[path.Join(fsys.prefix, name)] = info.Mode()
		})
	}
	return info, err
}

func (fsys recordingFS) ReadDir(name string) ([]iofs.DirEntry, error) {
	entries, err := iofs.ReadDir(fsys.base, name)
	if err == nil {
		recorded := make([]frozenEmbedEntry, 0, len(entries))
		for _, entry := range entries {
			recorded = append(recorded, frozenEmbedEntry{Name: entry.Name(), Mode: entry.Type()})
		}
		fsys.recorder.record(fsys.space, func(files frozenEmbedFS) {
			files.Directories[path.Join(fsys.prefix, name)] = recorded
		})
	}
	return entries, err
}

func (fsys frozenEmbedFS) Open(name string) (iofs.File, error) {
	if data, ok := fsys.Files[name]; ok {
		return fstest.MapFS{name: {Data: data, Mode: fsys.Stats[name]}}.Open(name)
	}
	if mode, ok := fsys.Stats[name]; ok && mode.IsDir() {
		return fstest.MapFS{name: {Mode: mode}}.Open(name)
	}
	return nil, &iofs.PathError{Op: "open", Path: name, Err: iofs.ErrNotExist}
}

func (fsys frozenEmbedFS) Stat(name string) (iofs.FileInfo, error) {
	if mode, ok := fsys.Stats[name]; ok {
		return fstest.MapFS{name: {Data: fsys.Files[name], Mode: mode}}.Stat(name)
	}
	return nil, &iofs.PathError{Op: "stat", Path: name, Err: iofs.ErrNotExist}
}

func (fsys frozenEmbedFS) ReadDir(name string) ([]iofs.DirEntry, error) {
	entries, ok := fsys.Directories[name]
	if !ok {
		return nil, &iofs.PathError{Op: "readdir", Path: name, Err: iofs.ErrNotExist}
	}
	files := make(fstest.MapFS, len(entries))
	for _, entry := range entries {
		files[entry.Name] = &fstest.MapFile{Mode: entry.Mode}
	}
	return files.ReadDir(".")
}
