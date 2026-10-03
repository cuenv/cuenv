package schema

import "strings"

// =============================================================================
// Infrastructure — infrastructure as code through Terraform provider plugins
// =============================================================================
//
// cuenv launches unmodified Terraform provider binaries and drives them over
// the Terraform plugin protocol (versions 5 and 6) using gRPC. Every managed
// resource is stored as its own record in a remote Turso (libSQL) database,
// keyed by the CUE module path (tenant), project name (discriminator) and
// selected named environment. Legacy no-flag configuration has a distinct
// state namespace.
//
// Typed configuration: `cuenv infrastructure provider add <source>@<version>`
// generates CUE types from the provider binary's own schema into
// `cue.mod/gen/<hostname>/<namespace>/<type>` and pins the release in
// `cuenv.lock`. Use the generated `#Provider` as the provider declaration and
// each `resources/<type>` package's `#Resource` as a resource declaration to
// have CUE reject unknown and mistyped arguments before any provider runs.
// `cuenv sync infrastructure` regenerates them from `cuenv.lock`.
//
// Semantic checks: this file reports a provider that sets both or neither of
// `version` and `path`, a `dependsOn` entry or `provider` that names nothing
// declared, and an invalid Turso URL, with `error()`. Each check applies to
// the top-level configuration and to every `environments.NAME` configuration,
// so a mistake in an environment that is not selected is still found. A check
// reads only the part of the configuration it needs and runs only when that
// part has no error of its own: when a provider, a resource or a nested
// `configuration` is invalid, the CUE error for that value is the only one
// reported for it, and the checks that read it wait until it is fixed. The
// `error()` builtin needs CUE language v0.14 or later in the module that
// holds this file; a project that depends on this schema through a registry
// is not affected by its own module's language version. The Rust engine
// repeats the checks for projects that do not unify with this schema.
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
	// Where managed resource state lives. One state database serves the
	// top-level configuration and every named environment; each has its own
	// identity inside it (module path, project name and environment).
	state!: {#InfrastructureState}

	// Named environments, selected with `--env NAME`. A selected environment
	// supplies its complete provider and resource set: nothing is inherited
	// from the top-level `providers`, `resources` or `providerEnvironment`.
	// The common state backend remains above the environment selection.
	environments?: [#InfrastructureName]: {#InfrastructureConfiguration}

	// Top-level configuration, used when no `--env` is given.
	#InfrastructureConfiguration
})

// The complete provider and resource set of one configuration: the top level
// of `infrastructure`, or one entry of `infrastructure.environments`. Every
// semantic check below reads only this configuration's own maps, so each
// configuration is checked on its own.
#InfrastructureConfiguration: close({
	// Provider plugins keyed by local name. Resource types default to the
	// provider named by their prefix (`random_pet` → `random`). The field is
	// always present (an empty struct when no provider is declared), so the
	// checks below can tell "no providers" from "providers that failed to
	// evaluate".
	providers: [#InfrastructureName]: {#InfrastructureProvider}

	// Managed resources keyed by name. The state address is `type.name`.
	// Every `dependsOn` entry must name a resource in this configuration, and
	// the resource's provider (explicit, or the type prefix) must be declared
	// in this configuration's `providers`.
	resources?: [#InfrastructureName]: {#ManagedResource}

	// What provider processes inherit from the cuenv process environment.
	//
	//   "inherit"  the ambient environment, minus the credentials of cuenv's
	//              own secret resolvers, plus the project's variables that the
	//              action's policy allows;
	//   "isolated" an empty environment except PATH, HOME, proxy and TLS
	//              variables, plus the project's variables that the action's
	//              policy allows.
	providerEnvironment?: *"inherit" | "isolated"

	// Semantic checks. They live beside `providers` and `resources`, not
	// inside them: an `error()` inside a provider or a resource makes that
	// whole struct an error, which would hide every name the other checks
	// read. Each check runs only when the struct it reads is itself free of
	// errors (`!= _|_`), so an invalid provider or resource reports its own
	// error and nothing is reported on its behalf. Name sets are built once
	// and every per-resource condition is a plain lookup, so the work grows
	// linearly with the number of resources. The checks compare names only:
	// looking a name up by value would evaluate the target, turning a
	// dependency cycle (which cuenv reports itself) or an unrelated error in
	// the target into a misleading "no resource named" error. Each failure is
	// an `error()` under `_unresolved`, keyed by the path of the offending
	// field inside this configuration, naming the missing resource or
	// provider without repeating any other value. CUE prefixes the path of the
	// configuration itself (`infrastructure` or
	// `infrastructure.environments.NAME`).
	let declaredResources = {if resources != _|_ for name, _ in resources {(name): true}}
	let declaredProviders = {if providers != _|_ for name, _ in providers {(name): true}}

	// Exactly one of `version` and `path` per provider. Presence is decided
	// from the field names a provider has, not from their values: a value
	// that is still open (`version: string`, filled in by another file or an
	// overlay) is present, and an invalid value reports its own error instead
	// of a second "set version or path". The check waits for `source`, which
	// has its own error when it is missing.
	if providers != _|_ for name, provider in providers {
		let fields = [for field, _ in provider {field}]
		let hasSource = len([for field in fields if field == "source" {field}]) > 0
		let hasVersion = len([for field in fields if field == "version" {field}]) > 0
		let hasPath = len([for field in fields if field == "path" {field}]) > 0
		if hasSource if !hasVersion if !hasPath {
			_unresolved: "providers.\(name)": error("set `version` (an exact registry release) or `path` (a local provider binary)")
		}
		if hasVersion if hasPath {
			_unresolved: "providers.\(name)": error("set exactly one of `version` and `path`, not both")
		}
	}

	// References between declarations.
	if resources != _|_ for name, resource in resources {
		if resource.dependsOn != _|_ for index, dependency in resource.dependsOn if (dependency =~ "") if declaredResources[dependency] == _|_ {
			_unresolved: "resources.\(name).dependsOn[\(index)]": error("no resource named \"\(dependency)\" in `resources` of this configuration")
		}
		if providers != _|_ {
			if resource.provider != _|_ if (resource.provider =~ "") if declaredProviders[resource.provider] == _|_ {
				_unresolved: "resources.\(name).provider": error("no provider named \"\(resource.provider)\" in `providers` of this configuration")
			}
			if resource.provider == _|_ if resource.type != _|_ if declaredProviders[strings.SplitN(resource.type, "_", 2)[0]] == _|_ {
				_unresolved: "resources.\(name).type": error("no provider named \"\(strings.SplitN(resource.type, "_", 2)[0])\" (the prefix of type \"\(resource.type)\") in `providers` of this configuration; declare it or set `provider`")
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
	//   libsql://, https:// or wss:// followed by a DNS name (whose last
	//   label starts with a letter), a dotted decimal IPv4 address (four
	//   octets from 0 to 255 without leading zeros) or a bracketed IPv6
	//   address in its standard text form;
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
// URL independently. The schema accepts a subset of what the store accepts and
// never a URL the store rejects, so a project that evaluates here cannot fail
// later on its database address. Spellings the store reads leniently are
// refused here: zero-padded, octal, hexadecimal and shorthand IPv4 hosts
// (`010.0.0.1`, `0x7f.1`, `127.1`, `2130706433`), a host name whose last label
// starts with a digit, an empty label, a trailing dot and an IPv6 zone.
_tursoOctet:        "(25[0-5]|2[0-4][0-9]|1[0-9]{2}|[1-9]?[0-9])"
_tursoPort:         "(:(6553[0-5]|655[0-2][0-9]|65[0-4][0-9]{2}|6[0-4][0-9]{3}|[1-5][0-9]{4}|[1-9][0-9]{0,3}))?"
_tursoPath:         "(/[A-Za-z0-9._~!$&'()*+,;=:@%/-]*)?"
_tursoIPv4:         "\(_tursoOctet)(\\.\(_tursoOctet)){3}"
_tursoLoopback4:    "127\\.\(_tursoOctet)\\.\(_tursoOctet)\\.\(_tursoOctet)"
_tursoGroup:        "[0-9A-Fa-f]{1,4}"
_tursoIPv6:         "((\(_tursoGroup):){7}\(_tursoGroup)|(\(_tursoGroup):){1,7}:|(\(_tursoGroup):){1,6}:\(_tursoGroup)|(\(_tursoGroup):){1,5}(:\(_tursoGroup)){1,2}|(\(_tursoGroup):){1,4}(:\(_tursoGroup)){1,3}|(\(_tursoGroup):){1,3}(:\(_tursoGroup)){1,4}|(\(_tursoGroup):){1,2}(:\(_tursoGroup)){1,5}|\(_tursoGroup):(:\(_tursoGroup)){1,6}|:((:\(_tursoGroup)){1,7}|:)|::((?i:ffff)(:0{1,4})?:)?\(_tursoIPv4)|(\(_tursoGroup):){1,4}:\(_tursoIPv4))"
_tursoLabel:        "[A-Za-z0-9]([A-Za-z0-9-]*[A-Za-z0-9])?"
_tursoName:         "(\(_tursoLabel)\\.)*[A-Za-z]([A-Za-z0-9-]*[A-Za-z0-9])?"
_tursoHost:         "(\(_tursoName)|\(_tursoIPv4)|\\[\(_tursoIPv6)\\])"
_tursoLoopback:     "((?i:localhost)|\(_tursoLoopback4)|\\[::1\\]|\\[(?i:::ffff:)\(_tursoLoopback4)\\])"
_tursoEncryptedUrl: "^(?i:libsql|https|wss)://\(_tursoHost)\(_tursoPort)\(_tursoPath)$"
_tursoLoopbackUrl:  "^(?i:http|ws)://\(_tursoLoopback)\(_tursoPort)\(_tursoPath)$"

#InfrastructureProvider: close({
	// Registry source address: "namespace/type" or
	// "hostname[:port]/namespace/type".
	source!: string & =~"^([a-zA-Z0-9.-]+(:[0-9]{1,5})?/)?[a-zA-Z0-9-]+/[a-zA-Z0-9-]+$"

	// Exact version to install from the registry. Exactly one of `version`
	// and `path` must be set; version constraints are not supported. A
	// generated `#Provider` sets it to the version its types describe.
	version?: string & =~"^(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\\+[0-9A-Za-z.-]+)?$"

	// Local provider binary, absolute or relative to the project directory.
	path?: string & !=""

	// Digest of the provider schema the project's generated CUE types were
	// made from. Set by a generated `#Provider`; the engine refuses a
	// provider whose schema has a different digest.
	schemaDigest?: string & =~"^sha256:[0-9a-f]{64}$"

	// Provider configuration block, validated by the provider's schema. A
	// generated `#Provider` types it with its `#Configuration`.
	configuration?: {...}
})

#ManagedResource: close({
	// Managed resource type, for example "random_pet".
	type!: string & =~"^[a-z][a-z0-9]*(_[a-z0-9]+)+$"

	// Local provider name; defaults to the type prefix.
	provider?: #InfrastructureName

	// Resources that must be applied before this one (and destroyed after).
	dependsOn?: [...#InfrastructureName]

	// Resource arguments, validated by the provider's schema. A generated
	// `#Resource` types them with its `#Configuration`.
	configuration?: {...}
})
