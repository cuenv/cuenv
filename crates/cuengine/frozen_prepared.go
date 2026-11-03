package main

/*
#include <stdlib.h>
*/
import "C"

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"sync"

	"cuelang.org/go/cue"
)

// The process key authenticates stateless provenance capabilities crossing the
// C boundary. It owns no snapshot, CUE context, runtime value or session handle.
// Capabilities are intentionally unusable in another process.
var preparedKey = sync.OnceValues(func() ([]byte, error) {
	key := make([]byte, 32)
	_, err := rand.Read(key)
	return key, err
})

type preparationOptions struct {
	ReferencePaths []valuePath   `json:"referencePaths"`
	SourcePaths    *[]valuePath  `json:"sourcePaths"`
	IgnoredPaths   []valuePath   `json:"ignoredPaths"`
	IgnoredGroups  [][]valuePath `json:"ignoredGroups"`
	Projections    []valuePath   `json:"projections"`
}

type preparedProjection struct {
	Path       valuePath         `json:"path"`
	References []frozenReference `json:"references"`
}

type preparedProvenance struct {
	Version        int                  `json:"version"`
	SnapshotDigest [sha256.Size]byte    `json:"snapshotDigest"`
	Scope          preparationOptions   `json:"scope"`
	Projections    []preparedProjection `json:"projections"`
}

type preparedCapability struct {
	Payload   json.RawMessage `json:"payload"`
	Signature []byte          `json:"signature"`
}

type preparedSpecializationOptions struct {
	Fills      []scalarFill    `json:"fills"`
	Projection valuePath       `json:"projection"`
	Deferred   []deferredValue `json:"deferred"`
}

//export cue_prepare_frozen
func cue_prepare_frozen(snapshot, options *C.char) *C.char {
	return C.CString(prepareFrozenResponse(C.GoString(snapshot), C.GoString(options)))
}

func prepareFrozenResponse(snapshot, optionsJSON string) (result string) {
	defer func() {
		if recover() != nil {
			result = frozenFailure(ErrorCodePanicRecover, "CUE preparation failed")
		}
	}()
	proof, err := prepareFrozen(snapshot, optionsJSON)
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "CUE provenance could not be prepared")
	}
	encoded, err := json.Marshal(proof)
	if err != nil {
		return frozenFailure(ErrorCodeJSONMarshal, "CUE preparation could not be encoded")
	}
	return createSuccessResponse(string(encoded))
}

func prepareFrozen(snapshot, optionsJSON string) (string, error) {
	var graph frozenGraph
	var options preparationOptions
	if json.Unmarshal([]byte(snapshot), &graph) != nil || json.Unmarshal([]byte(optionsJSON), &options) != nil {
		return "", errors.New("invalid preparation options")
	}
	base, err := graph.build()
	if err != nil {
		return "", err
	}
	// Only these roots authorize global alias uses. Requested projections may
	// never expand that set, even when contained in a configured root.
	tracked := make([]frozenReference, 0)
	roots := make([]cue.Path, 0, len(options.ReferencePaths))
	for _, typed := range options.ReferencePaths {
		path, err := typed.cuePath()
		if err != nil {
			return "", err
		}
		roots = append(roots, path)
		references, err := frozenReferences(base, path)
		if err != nil {
			return "", err
		}
		tracked = append(tracked, references...)
	}
	if err := validateUntrackedDependencies(dependencyInspection{instance: base, references: tracked, scope: frozenDependencyScope{SourcePaths: options.SourcePaths, IgnoredPaths: options.IgnoredPaths, IgnoredGroups: options.IgnoredGroups}}); err != nil {
		return "", err
	}
	rootReferences := make(map[string]bool)
	for _, reference := range tracked {
		encoded, err := json.Marshal(reference)
		if err != nil {
			return "", err
		}
		rootReferences[string(encoded)] = true
	}
	proof := preparedProvenance{Version: 1, SnapshotDigest: sha256.Sum256([]byte(snapshot)), Scope: options}
	seen := make(map[string]bool)
	for _, typed := range options.Projections {
		path, err := typed.cuePath()
		if err != nil || !base.LookupPath(path).Exists() || seen[path.String()] {
			return "", errors.New("invalid prepared projection")
		}
		seen[path.String()] = true
		covered := false
		for _, root := range roots {
			if pathWithin(path, root) {
				covered = true
				break
			}
		}
		if !covered {
			return "", errors.New("prepared projection is outside reference roots")
		}
		references, err := frozenReferences(base, path)
		if err != nil {
			return "", err
		}
		for _, reference := range references {
			encoded, err := json.Marshal(reference)
			if err != nil || !rootReferences[string(encoded)] {
				return "", errors.New("projection references were not checked by roots")
			}
			if reference.PrimitiveType == "" {
				continue
			}
			if !reference.Supported || !preparedSourceAllowed(reference.Target, options.SourcePaths) {
				return "", errors.New("projection source is not authorized")
			}
		}
		proof.Projections = append(proof.Projections, preparedProjection{Path: typed, References: references})
	}
	payload, err := json.Marshal(proof)
	if err != nil {
		return "", err
	}
	key, err := preparedKey()
	if err != nil {
		return "", err
	}
	signature := hmac.New(sha256.New, key)
	signature.Write(payload)
	encoded, err := json.Marshal(preparedCapability{Payload: payload, Signature: signature.Sum(nil)})
	return string(encoded), err
}

func preparedSourceAllowed(target valuePath, sources *[]valuePath) bool {
	if sources == nil {
		return true
	}
	parsed, err := target.cuePath()
	if err != nil {
		return false
	}
	for _, source := range *sources {
		path, err := source.cuePath()
		if err == nil && pathWithin(parsed, path) {
			return true
		}
	}
	return false
}

//export cue_specialize_prepared
func cue_specialize_prepared(snapshot, capability, options *C.char) *C.char {
	return C.CString(specializePreparedResponse(C.GoString(snapshot), C.GoString(capability), C.GoString(options)))
}

func specializePreparedResponse(snapshot, capability, optionsJSON string) (result string) {
	defer func() {
		if recover() != nil {
			result = frozenFailure(ErrorCodePanicRecover, "Prepared CUE specialization failed")
		}
	}()
	value, err := specializePrepared(snapshot, capability, optionsJSON)
	if err != nil {
		return frozenFailure(ErrorCodeBuildValue, "Prepared CUE specialization failed validation")
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return frozenFailure(ErrorCodeJSONMarshal, "Prepared CUE specialization could not be encoded")
	}
	return createSuccessResponse(string(encoded))
}

func specializePrepared(snapshot, capability, optionsJSON string) (*specializationResult, error) {
	var token preparedCapability
	if json.Unmarshal([]byte(capability), &token) != nil {
		return nil, errors.New("invalid prepared capability")
	}
	key, err := preparedKey()
	if err != nil {
		return nil, err
	}
	signature := hmac.New(sha256.New, key)
	signature.Write(token.Payload)
	if !hmac.Equal(token.Signature, signature.Sum(nil)) {
		return nil, errors.New("invalid prepared capability")
	}
	var proof preparedProvenance
	var options preparedSpecializationOptions
	if json.Unmarshal(token.Payload, &proof) != nil || json.Unmarshal([]byte(optionsJSON), &options) != nil || proof.Version != 1 || proof.SnapshotDigest != sha256.Sum256([]byte(snapshot)) {
		return nil, errors.New("invalid prepared specialization")
	}
	projection, err := options.Projection.cuePath()
	if err != nil {
		return nil, err
	}
	var selected *preparedProjection
	for index := range proof.Projections {
		path, err := proof.Projections[index].Path.cuePath()
		if err == nil && samePath(path, projection) {
			selected = &proof.Projections[index]
			break
		}
	}
	if selected == nil {
		return nil, errors.New("projection was not prepared")
	}
	// Authority is projection-local, never the union of sources used by other
	// prepared consumers. Deferred pairs are checked by the shared strict path.
	allowed := make(map[string]bool)
	for _, reference := range selected.References {
		if reference.Supported && reference.PrimitiveType != "" {
			path, err := reference.Target.cuePath()
			if err != nil {
				return nil, err
			}
			allowed[path.String()] = true
		}
	}
	for _, fill := range options.Fills {
		path, err := fill.Path.cuePath()
		if err != nil || !allowed[path.String()] {
			return nil, errors.New("source was not prepared for this projection")
		}
	}
	var graph frozenGraph
	if json.Unmarshal([]byte(snapshot), &graph) != nil {
		return nil, errors.New("invalid frozen graph")
	}
	base, err := graph.build()
	if err != nil {
		return nil, err
	}
	return specializeWithReferences(base, specializationOptions{Fills: options.Fills, Projection: options.Projection, Deferred: options.Deferred}, selected.References)
}
