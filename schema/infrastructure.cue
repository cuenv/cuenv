package schema

import "strings"

// A project-local .infrastructure.cue file selects the separately evaluated
// infrastructure package. The directory is relative to the project directory.
#InfrastructureLocator: {
	directory!: string & !=""
	"package"!: string & !=""
	projectPackage?: string & !=""
}

// =============================================================================
// Infrastructure — infrastructure as code through Terraform provider plugins
// =============================================================================
//
// cuenv launches unmodified Terraform provider binaries and drives them over
// the Terraform plugin protocol (versions 5 and 6) using gRPC. Every managed
// resource is stored as its own record in a remote SQLite database (Turso or
// Cloudflare D1),
// keyed by the CUE module path (tenant), project name (discriminator) and
// selected named environment. Legacy no-flag configuration has a distinct
// state namespace.
//
// Typed configuration: `cuenv infrastructure provider add <source>@<version>`
// generates CUE types from the provider binary's own schema into
// `cue.mod/gen/<hostname>/<namespace>/<type>` and pins the release in
// `cuenv.lock`. Use the generated `#Provider` as the provider declaration and
// each `resources/<type>` package's `#Resource` as a resource declaration, or
// `data/<type>` package's `#DataSource` for a lookup, to have CUE reject unknown
// and mistyped arguments before any provider runs.
// `cuenv sync infrastructure` regenerates them from `cuenv.lock`.
//
// Semantic checks: this file reports a provider that sets both or neither of
// `version` and `path`, a `dependsOn` entry or `provider` that names nothing
// declared, a state block that names no backend or two, and an invalid Turso
// URL, with `error()`. Each check applies to
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
// Proof of concept: top-level primitive arguments can refer to another
// resource or data source's primitive attribute with #InfrastructureOutputReference.
// A sensitive source attribute may be referenced: plans show the destination
// argument as (sensitive). Known sensitive strings of at least four characters
// are registered for output redaction; numeric and boolean values are only masked.
// Use `dependsOn` when only ordering is needed.
//
// Closedness: every nested definition is referenced inside a struct literal
// (`{#InfrastructureState}` rather than `#InfrastructureState`). A project
// file usually embeds `schema.#Project` at file level, which opens the
// project struct; the struct literals keep each nested definition closed
// there too, so a misspelled field (`resource:`, `sourcee:`) is an error
// instead of an ignored extra field.

#Infrastructure: {
	// Where managed resource state lives. One state backend serves the
	// top-level configuration and every named environment; each has its own
	// identity inside it (module path, project name and environment, and
	// for `s3` the CUE package too).
	state!: {#InfrastructureState}

	// Exactly one state backend. Presence is decided from the field names,
	// so a backend with an error of its own (an invalid Turso URL, a
	// malformed D1 identifier) reports only that error.
	if state != _|_ {
		let backends = [for field, _ in state {field}]
		if len(backends) == 0 {
			_unresolved: state: error("set one state backend: `turso`, `d1` or `s3`")
		}
		if len(backends) > 1 {
			_unresolved: state: error("set exactly one state backend: `turso`, `d1` or `s3`")
		}
	}

	// Where `cuenv infrastructure state migrate` copies this project's
	// state to, while a migration is under way. Keep `state` naming the
	// backend the state is in now; once every environment is migrated,
	// make this the `state` and remove `migrateStateTo`. `plan` and `apply`
	// warn while it is set.
	migrateStateTo?: {#InfrastructureState}

	if migrateStateTo != _|_ {
		let targets = [for field, _ in migrateStateTo {field}]
		if len(targets) != 1 {
			_unresolved: migrateStateTo: error("set exactly one migration target: `turso`, `d1` or `s3`")
		}
	}

	// Named environments, selected with `--env NAME`. A selected environment
	// supplies its complete provider, resource and data source set: nothing is
	// inherited from top-level `providers`, `resources`, `data` or `providerEnvironment`.
	// The common state backend remains above the environment selection.
	environments?: [#InfrastructureName]: {#InfrastructureConfiguration}

	// CI workflows that `cuenv sync ci` emits for this infrastructure, for
	// the project's `ci.providers`. Only the top-level configuration is
	// planned and applied.
	workflows?: {#InfrastructureWorkflows}

	// Top-level configuration, used when no `--env` is given.
	#InfrastructureConfiguration
}

// CI workflows for infrastructure: `cuenv infrastructure plan` on pull
// requests that touch the project, and `cuenv infrastructure apply --yes` on
// `apply.on`. They are emitted for the project's `ci.providers` (only GitHub
// Actions so far) and reuse its `ci` contributors (cuenv install, secret
// providers).
#InfrastructureWorkflows: {
	plan: {
		// Post the plan as a pull request comment. Each project keeps one
		// comment, which later runs edit in place.
		comment: *true | bool
	}

	apply: {
		// What triggers an apply:
		//   "push"    pushes to `branch`
		//   "tag"     pushed tags matching `tags`
		//   "release" published GitHub releases
		//   "manual"  manual runs (`workflow_dispatch`) only
		on: *"push" | "tag" | "release" | "manual"

		// The branch whose pushes apply, when `on` is "push".
		branch: *"main" | (string & !="")

		// Tag patterns whose pushes apply, when `on` is "tag".
		tags: *["v*"] | [string, ...string]

		// Also allow manual runs when `on` is "push", "tag" or "release".
		manual: *true | bool

		// GitHub environment for the apply job, for example to require a
		// reviewer's approval before it runs.
		environment?: string
	}
}

// The complete provider, resource and data source set of one configuration: the top level
// of `infrastructure`, or one entry of `infrastructure.environments`. Every
// semantic check below reads only this configuration's own maps, so each
// configuration is checked on its own.
#InfrastructureConfiguration: {
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

	// Read-only provider lookups. Results are read for each plan/apply and
	// are never persisted as managed resource state.
	data?: [#InfrastructureName]: {#DataSource}

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
	if data != _|_ for name, source in data {
		if source.dependsOn != _|_ for index, dependency in source.dependsOn if (dependency =~ "") if declaredResources[dependency] == _|_ {
			_unresolved: "data.\(name).dependsOn[\(index)]": error("no resource named \"\(dependency)\" in `resources` of this configuration")
		}
		if providers != _|_ {
			if source.provider != _|_ if (source.provider =~ "") if declaredProviders[source.provider] == _|_ {
				_unresolved: "data.\(name).provider": error("no provider named \"\(source.provider)\" in `providers` of this configuration")
			}
			if source.provider == _|_ if source.type != _|_ if declaredProviders[strings.SplitN(source.type, "_", 2)[0]] == _|_ {
				_unresolved: "data.\(name).type": error("no provider named \"\(strings.SplitN(source.type, "_", 2)[0])\" (the prefix of type \"\(source.type)\") in `providers` of this configuration; declare it or set `provider`")
			}
		}
	}
}

#InfrastructureName: string & =~"^[a-zA-Z][a-zA-Z0-9_-]*$"

// Exactly one state backend. Turso and D1 keep the same tables, so a
// database written through one is never read through the other by accident:
// each backend names its own database. `s3` keeps one object per project and
// environment in a bucket.
#InfrastructureState: {
	// A Turso (libSQL) database, or a local `sqld`.
	turso?: {#TursoState}

	// A Cloudflare D1 database, reached through the Cloudflare API.
	d1?: {#D1State}

	// An S3-compatible bucket: AWS S3, Cloudflare R2 or MinIO.
	s3?: {#S3State}
}

// An S3-compatible bucket. Each project's state is one object,
// `[<root>/]<module>/_<package>/<project>/<env | _default>/state.json`,
// written with conditional requests, so the bucket needs no other service
// for locking. Anyone who can read the bucket can read every project's
// state, including provider secrets in plaintext, and anyone who can write
// it can change every project's state: use one bucket (or one prefix-scoped
// credential) per trust boundary.
#S3State: {
	// Bucket name: 3 to 63 lowercase letters, digits, dots and hyphens. A
	// name with a dot needs path addressing.
	bucket!: string & =~"^[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$"

	// Signing region, such as "eu-west-2". Cloudflare R2 uses "auto", and
	// then `endpoint` is required.
	region!: string & =~"^[a-z0-9-]+$"

	// Endpoint URL: https:// with a host name or address and an optional
	// port, or http:// only for a loopback host (localhost, 127.x.y.z,
	// [::1]), since state holds provider secrets. No path, credentials,
	// query or fragment. Defaults to https://s3.<region>.amazonaws.com. For
	// R2: https://<ACCOUNT_ID>.r2.cloudflarestorage.com.
	endpoint?: string & (=~_s3EncryptedEndpoint | =~_s3LoopbackEndpoint)

	// Key prefix for every object of this configuration, so several
	// configurations can share a bucket: lowercase segments separated by
	// "/".
	root?: string & =~"^[a-z0-9][a-z0-9_-]*(/[a-z0-9][a-z0-9_-]*)*$"

	// How requests name the bucket. "auto" uses path style for an IP
	// address, localhost or a bucket name with a dot, and virtual-hosted
	// style otherwise.
	addressing: *"auto" | "virtual-hosted" | "path"

	// Credentials, written like a state token. No ambient credential chain
	// is read: name `AWS_ACCESS_KEY_ID` and friends with #EnvPassthrough to
	// use them.
	accessKeyId!:     #InfrastructureStateToken
	secretAccessKey!: #InfrastructureStateToken
	sessionToken?:    #InfrastructureStateToken

	// Least time between two writes of one project's state, such as "1s"
	// or "250ms". Defaults to "1s" for R2, which accepts about one write
	// per second to a key, and to none elsewhere.
	minWriteInterval?: string & =~"^[0-9]+(ms|s)$"

	// How many earlier versions of each project's state to keep, written
	// when a lock that changed something is released. 0 keeps none and
	// deletes those kept so far. Restore one with
	// `cuenv infrastructure state restore`.
	history: *10 | int & >=0 & <=1000

	if addressing == "virtual-hosted" if bucket =~ "\\." {
		_unresolved: addressing: error("a bucket name with a dot needs `addressing: \"path\"` (or \"auto\")")
	}
	if region == "auto" if endpoint == _|_ {
		_unresolved: endpoint: error("`region: \"auto\"` (Cloudflare R2) needs `endpoint`")
	}
}

#TursoState: {
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

	// Database authentication token. Optional: a local `sqld` may need
	// none, and cuenv then connects without one.
	token?: #InfrastructureStateToken
}

#D1State: {
	// Cloudflare account ID: the 32 lowercase hexadecimal digits shown in
	// the dashboard and by `wrangler whoami`.
	accountId!: string & =~"^[0-9a-f]{32}$"

	// D1 database ID: the lowercase UUID `wrangler d1 list` prints (not the
	// database name).
	databaseId!: string & =~"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"

	// Cloudflare API token with D1 Edit access. Required: the Cloudflare
	// API refuses every request without one.
	token!: #InfrastructureStateToken
}

// A state backend's authentication token, written like a project environment
// variable: a literal string, a #Secret resolved when the command runs, an
// interpolation of strings and secrets, or #EnvPassthrough with a `name` to
// read that variable from cuenv's own environment (a CI secret, for
// instance). Nothing is read from the environment unless it is named here.
// Integers, booleans and policies do not apply to a token. One state backend
// serves the top level and every environment, so each credential is set
// once.
//
// cuenv redacts the resolved token from all output and never passes it to a
// provider; a passthrough variable is withheld from providers under its name.
// The passthrough arm is an inline struct, not a definition, so it is closed
// explicitly. An open arm would accept extra fields.
#InfrastructureStateToken: string | #Secret | #InterpolatedEnv | close({
	cuenvPassthrough: true
	name!:            string & =~"^[A-Za-z_][A-Za-z0-9_]*$"
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

// The S3 endpoint reuses the Turso host grammar without a path.
_s3EncryptedEndpoint: "^(?i:https)://\(_tursoHost)\(_tursoPort)/?$"
_s3LoopbackEndpoint:  "^(?i:http)://((?i:localhost)|\(_tursoLoopback4)|\\[::1\\])\(_tursoPort)/?$"

#InfrastructureProvider: {
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
}

#ManagedResource: {
	// Managed resource type, for example "random_pet".
	type!: string & =~"^[a-z][a-z0-9]*(_[a-z0-9]+)+$"

	// Local provider name; defaults to the type prefix.
	provider?: #InfrastructureName

	// Resources that must be applied before this one (and destroyed after).
	dependsOn?: [...#InfrastructureName]

	// Abstract top-level scalar outputs. Generated provider types declare the
	// available attributes; untyped resources declare each selected attribute
	// explicitly, for example `outputs: id: string`. Values come from provider
	// state at runtime and are never sent as provider configuration.
	outputs?: {[string]: string | number | bool}

	// Resource arguments, validated by the provider's schema. A generated
	// `#Resource` types them with its `#Configuration`.
	configuration?: {...}
}

// A read-only lookup through a provider's data source schema. `dependsOn`
// names managed resources; references infer dependencies on either kind.
#DataSource: {
	#ManagedResource
}

// A top-level primitive argument supplied by a managed resource or data
// source. Exactly one source namespace must be selected.
#InfrastructureOutputReference: {
	fromResource!: string & =~"^[a-z][a-z0-9]*(_[a-z0-9]+)+\\.[a-zA-Z][a-zA-Z0-9_-]*$"
	attribute!:    string & =~"^[a-zA-Z_][a-zA-Z0-9_]*$"
} | {
	fromDataSource!: string & =~"^[a-z][a-z0-9]*(_[a-z0-9]+)+\\.[a-zA-Z][a-zA-Z0-9_-]*$"
	attribute!:      string & =~"^[a-zA-Z_][a-zA-Z0-9_]*$"
}
