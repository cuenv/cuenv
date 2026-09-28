package schema

import "strings"

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
//
// Closedness: every nested definition is referenced inside a struct literal
// (`{#InfrastructureState}` rather than `#InfrastructureState`). A project
// file usually embeds `schema.#Project` at file level, which opens the
// project struct; the struct literals keep each nested definition closed
// there too, so a misspelled field (`resource:`, `sourcee:`) is an error
// instead of an ignored extra field.

#Infrastructure: close({
	// Where managed resource state lives.
	state!: {#InfrastructureState}

	// Provider plugins keyed by local name. Resource types default to the
	// provider named by their prefix (`random_pet` → `random`).
	providers?: [#InfrastructureName]: {#InfrastructureProvider}

	// Managed resources keyed by name. The state address is `type.name`.
	// Every `dependsOn` entry must name a resource here, and the resource's
	// provider (explicit, or the type prefix) must be declared in `providers`.
	resources?: [#InfrastructureName]: {#ManagedResource}

	// Reference checks. They live outside `resources` and read only names,
	// `type`, `provider` and `dependsOn`, so the name sets are built once and
	// the checks stay linear in the number of resources. They compare names
	// only: looking a name up by value would evaluate the target, turning a
	// dependency cycle (which cuenv reports itself) or an unrelated error in
	// the target into a misleading "no resource named" error. Each failure is
	// an `error()` under `_unresolved`, keyed by the path of the offending
	// field, naming the missing resource or provider without repeating any
	// other value.
	let declaredResources = {if resources != _|_ for name, _ in resources {(name): true}}
	let declaredProviders = {if providers != _|_ for name, _ in providers {(name): true}}
	if resources != _|_ for name, resource in resources {
		if resource.dependsOn != _|_ for index, dependency in resource.dependsOn if (dependency =~ "") if declaredResources[dependency] == _|_ {
			_unresolved: "resources.\(name).dependsOn[\(index)]": error("no resource named \"\(dependency)\" in infrastructure.resources")
		}
		if resource.provider != _|_ if (resource.provider =~ "") if declaredProviders[resource.provider] == _|_ {
			_unresolved: "resources.\(name).provider": error("no provider named \"\(resource.provider)\" in infrastructure.providers")
		}
		if resource.provider == _|_ if resource.type != _|_ {
			let defaultProvider = strings.SplitN(resource.type, "_", 2)[0]
			if declaredProviders[defaultProvider] == _|_ {
				_unresolved: "resources.\(name).type": error("no provider named \"\(defaultProvider)\" (the prefix of type \"\(resource.type)\") in infrastructure.providers; declare it or set `provider`")
			}
		}
	}
})

#InfrastructureName: string & =~"^[a-zA-Z][a-zA-Z0-9_-]*$"

#InfrastructureState: close({
	turso!: {#TursoState}
})

#TursoState: close({
	// Database URL. Accepted forms (scheme and `localhost` in any case):
	//
	//   libsql://, https:// or wss:// followed by a DNS name, a dotted IPv4
	//   address or a bracketed IPv6 address;
	//   http:// or ws:// only for a loopback host — `localhost`, a dotted
	//   IPv4 address in 127.0.0.0/8, `[::1]` or `[::ffff:127.x.y.z]` — so
	//   the token never travels in cleartext to another machine;
	//
	// then an optional port from 1 to 65535 and an optional path. The URL must
	// not carry credentials, a query or a fragment. An invalid URL is reported
	// without repeating it, because it may hold a token.
	url!: string

	if url != _|_ if !(url =~ _tursoEncryptedUrl) if !(url =~ _tursoLoopbackUrl) {
		_invalidUrl: error("`url` must be libsql://, https:// or wss:// with a host name or address, or http:// or ws:// with a loopback host (localhost, 127.x.y.z, [::1]); an optional port from 1 to 65535 and path; no credentials, query, fragment or whitespace")
	}

	// Environment variable holding the database authentication token. The
	// default matches the name the Turso command line tool documents.
	authenticationTokenEnvironmentVariable: *"TURSO_AUTH_TOKEN" | (string & =~"^[A-Za-z_][A-Za-z0-9_]*$")
})

// Building blocks of the Turso URL contract. The Rust state store parses the
// URL independently and must accept exactly the same set.
_tursoOctet:        "(25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])"
_tursoPort:         "(:(6553[0-5]|655[0-2][0-9]|65[0-4][0-9]{2}|6[0-4][0-9]{3}|[1-5][0-9]{4}|[1-9][0-9]{0,3}))?"
_tursoPath:         "(/[A-Za-z0-9._~!$&'()*+,;=:@%/-]*)?"
_tursoLoopback4:    "127\\.\(_tursoOctet)\\.\(_tursoOctet)\\.\(_tursoOctet)"
_tursoHost:         "([A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?|\\[[0-9A-Fa-f:.]+\\])"
_tursoLoopback:     "((?i:localhost)|\(_tursoLoopback4)|\\[::1\\]|\\[(?i:::ffff:)\(_tursoLoopback4)\\])"
_tursoEncryptedUrl: "^(?i:libsql|https|wss)://\(_tursoHost)\(_tursoPort)\(_tursoPath)$"
_tursoLoopbackUrl:  "^(?i:http|ws)://\(_tursoLoopback)\(_tursoPort)\(_tursoPath)$"

#InfrastructureProvider: close({
	// Registry source address: "namespace/type" or
	// "hostname[:port]/namespace/type".
	source!: string & =~"^([a-zA-Z0-9.-]+(:[0-9]{1,5})?/)?[a-zA-Z0-9-]+/[a-zA-Z0-9-]+$"

	// Exact version to install from the registry. Exactly one of `version`
	// and `path` must be set; version constraints are not supported. When
	// `configuration` uses a github.com/cuenv/terraform module, its
	// `@v<major>` must match.
	version?: string & =~"^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\\+[0-9A-Za-z.-]+)?$"

	// Local provider binary, absolute or relative to the project directory.
	path?: string & !=""

	// Provider configuration block, validated by the provider's schema.
	// Unify with the provider module's `#ProviderConfig` for typing.
	configuration?: {...}

	// Exactly one of `version` and `path`. The check waits for `source`, so
	// the bare definition (and a provider still missing `source`, which has
	// its own error) does not report it.
	if source != _|_ if version == _|_ if path == _|_ {
		_versionOrPath: error("set `version` (an exact registry release) or `path` (a local provider binary)")
	}
	if version != _|_ if path != _|_ {
		_versionOrPath: error("set exactly one of `version` and `path`, not both")
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
