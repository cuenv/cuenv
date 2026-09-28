package schema

import (
	"list"
	"strings"
)

// =============================================================================
// Infrastructure — infrastructure as code through Terraform provider plugins
// =============================================================================
//
// cuenv launches unmodified Terraform provider binaries and drives them over
// the Terraform plugin protocol (versions 5 and 6) using gRPC. Every managed
// resource is stored as its own record in a remote Turso (libSQL) database,
// keyed by the CUE module path (tenant) and the project name (discriminator).
//
// Typed configuration: every provider's schema is published as a CUE module
// by https://github.com/cuenv/terraform at
// "github.com/cuenv/terraform/terraform/<namespace>/<type>@v<major>". Unify
// `configuration` with its `#ProviderConfig` and `#Resource_<type>`
// definitions to have CUE reject unknown and mistyped arguments before any
// provider runs.
//
// Proof of concept: resources cannot reference each other's attributes yet;
// order them with `dependsOn`.

#Infrastructure: close({
	// Where managed resource state lives.
	state!: #InfrastructureState

	// Provider plugins keyed by local name. Resource types default to the
	// provider named by their prefix (`random_pet` → `random`).
	providers?: [#InfrastructureName]: #InfrastructureProvider

	// Managed resources keyed by name. The state address is `type.name`.
	// Every `dependsOn` entry must name a resource here, and the resource's
	// provider (explicit, or the type prefix) must be declared in `providers`.
	//
	// The checks compare names only: looking a name up by value would
	// evaluate the target, turning a dependency cycle (which cuenv reports
	// itself) or an unrelated error in the target into a misleading "no
	// resource named" error.
	resources?: [#InfrastructureName]: Resource={
		#ManagedResource

		if Resource.dependsOn != _|_ for dependency in Resource.dependsOn if !list.Contains([for name, _ in resources {name}], dependency) {
			dependsOn: "no resource named \"\(dependency)\" in infrastructure.resources"
		}
		if Resource.provider != _|_ if !list.Contains(providerNames, Resource.provider) {
			provider: "no provider named \"\(Resource.provider)\" in infrastructure.providers"
		}
		if Resource.provider == _|_ if Resource.type != _|_ {
			let defaultProvider = strings.SplitN(Resource.type, "_", 2)[0]
			if !list.Contains(providerNames, defaultProvider) {
				type: "no provider named \"\(defaultProvider)\" (the prefix of type \"\(Resource.type)\") in infrastructure.providers; declare it or set `provider`"
			}
		}
	}

	let providerNames = [if providers != _|_ for name, _ in providers {name}]
})

#InfrastructureName: string & =~"^[a-zA-Z][a-zA-Z0-9_-]*$"

#InfrastructureState: close({
	turso!: #TursoState
})

#TursoState: close({
	// Database URL: libsql://<database>-<organization>.turso.io, https:// or
	// wss://. Plain http:// and ws:// are accepted only for a local sqld
	// server on a loopback address, so the token never travels in cleartext.
	// The URL must not carry credentials, a query or a fragment.
	url!: string & (=~"^(libsql|https|wss)://[^/\\s?#@:][^/\\s?#@]*(/[^\\s?#]*)?$" | =~"^(http|ws)://(localhost|127\\.[0-9]+\\.[0-9]+\\.[0-9]+|\\[::1\\])(:[0-9]+)?(/[^\\s?#]*)?$")

	// Environment variable holding the database authentication token. The
	// default matches the name the Turso command line tool documents.
	authenticationTokenEnvironmentVariable: *"TURSO_AUTH_TOKEN" | (string & =~"^[A-Za-z_][A-Za-z0-9_]*$")
})

#InfrastructureProvider: close({
	// Registry source address: "namespace/type" or "hostname/namespace/type".
	source!: string & =~"^([a-zA-Z0-9.-]+/)?[a-zA-Z0-9-]+/[a-zA-Z0-9-]+$"

	// Exact version to install from the registry. Exactly one of `version`
	// and `path` must be set; version constraints are not supported. When `configuration`
	// uses a github.com/cuenv/terraform module, its `@v<major>` must match.
	version?: string & =~"^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\\+[0-9A-Za-z.-]+)?$"

	// Local provider binary, absolute or relative to the project directory.
	path?: string

	// Provider configuration block, validated by the provider's schema.
	// Unify with the provider module's `#ProviderConfig` for typing.
	configuration?: {...}

	// Exactly one of `version` and `path`.
	if version == _|_ if path == _|_ {
		version!: _
	}
	if version != _|_ if path != _|_ {
		path: "set exactly one of `version` and `path`, not both"
	}
})

#ManagedResource: close({
	// Managed resource type, for example "random_pet".
	type!: string & =~"^[a-z][a-z0-9]*(_[a-z0-9]+)+$"

	// Local provider name; defaults to the type prefix.
	provider?: #InfrastructureName

	// Resources that must be applied before this one (and destroyed after).
	dependsOn?: [...#InfrastructureName]

	// Resource arguments, validated by the provider's schema. Unify with the
	// provider module's `#Resource_<type>` for typing.
	configuration?: {...}
})
