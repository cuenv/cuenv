package contributors

import "github.com/cuenv/cuenv/schema"

// #Infisical validates Infisical REST API secret resolution credentials.
//
// Active when:
// - Pipeline environment contains Infisical secret references
//   (resolver="infisical")
//
// Injects tasks:
// - cuenv:contributor:infisical.setup: Validates Infisical auth env for secret resolution
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Infisical]
#Infisical: schema.#Contributor & {
	id: "infisical"
	when: secretsProvider: ["infisical"]
	tasks: [{
		id:        "infisical.setup"
		label:     "Setup Infisical"
		priority:  20
		shell:     false
		dependsOn: ["cuenv.setup"]
		command:   "cuenv secrets setup infisical"
		env: {
			INFISICAL_CLIENT_ID:         "${INFISICAL_CLIENT_ID}"
			INFISICAL_CLIENT_SECRET:     "${INFISICAL_CLIENT_SECRET}"
			INFISICAL_TOKEN:             "${INFISICAL_TOKEN}"
			INFISICAL_ORGANIZATION_SLUG: "${INFISICAL_ORGANIZATION_SLUG}"
		}
	}]
}

// #InfisicalOidc lets cuenv log in to Infisical with GitHub Actions OIDC, so
// no Infisical credential is stored in GitHub (Pattern A: cuenv resolves
// #InfisicalSecret references itself).
//
// Add it next to #Infisical (or #DefaultContributors, which includes
// #Infisical); the order does not matter. On GitHub Actions it:
// - grants the job `id-token: write`;
// - before cuenv resolves secrets, requests the job's OIDC ID token, masks it
//   and writes it to $GITHUB_ENV as INFISICAL_AUTH_JWT, together with
//   INFISICAL_MACHINE_IDENTITY_ID (heredoc form, random delimiter; a value
//   with a line break fails the step). Later steps of the job can read the
//   JWT until it expires.
//
// cuenv's Infisical resolver then calls Infisical's OIDC Auth login with the
// identity ID and JWT. `cuenv ci` logs in once, before its first task, while
// the JWT is fresh, and never logs in again with the same JWT: set the
// identity's access token TTL to cover the whole run, or later secret reads
// fail (and say so). The resolver itself never talks to GitHub. Do not also
// provide INFISICAL_CLIENT_ID and INFISICAL_CLIENT_SECRET: configuring both
// login methods is an error.
//
// Parameters (definition fields, so set them by unification):
// - #identityId: the machine identity ID. Defaults to the
//   INFISICAL_MACHINE_IDENTITY_ID repository secret; a literal ID or a
//   `${{ vars.NAME }}` expression also works (identity IDs are not secret).
// - #audience: the `aud` claim to request. Empty (the default) keeps GitHub's
//   default audience, the repository owner's URL, like Infisical/secrets-action.
//   It must match an audience allowed by the identity's OIDC Auth configuration.
//
// GitHub only: other CI providers fail the step because they have no Actions
// ID token endpoint.
//
// Active when:
// - Pipeline environment contains Infisical secret references
//   (resolver="infisical")
//
// Injects tasks:
// - cuenv:contributor:infisical.oidc: Exports a GitHub OIDC token for Infisical
//
// Usage:
//
//	import (
//		"list"
//		xContributors "github.com/cuenv/cuenv/contrib/contributors"
//	)
//
//	ci: contributors: list.Concat([
//		xContributors.#DefaultContributors,
//		[xContributors.#InfisicalOidc & {#audience: "https://github.com/my-org"}],
//	])
#InfisicalOidc: schema.#Contributor & {
	// A line break would add a variable to $GITHUB_ENV; the step also rejects
	// one in the secret's value, which CUE cannot see.
	#identityId: string & !~"[\n\r]" | *"${INFISICAL_MACHINE_IDENTITY_ID}"

	// Single quotes and newlines would break out of the shell literal below.
	#audience: string & !~"['\n\r]" | *""

	id: "infisical-oidc"
	when: secretsProvider: ["infisical"]
	tasks: [{
		id:    "infisical.oidc"
		label: "Get GitHub OIDC token for Infisical"
		// Setup stage: after cuenv is installed (the JWT is short-lived, so it
		// is fetched as late as possible) and before infisical.setup (20).
		priority: 15
		ci: github: permissions: "id-token": "write"
		env: INFISICAL_MACHINE_IDENTITY_ID: #identityId
		script: """
			set -eu
			if [ -z "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || [ -z "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" ]; then
			  echo "::error::No GitHub Actions OIDC token endpoint: the job needs 'id-token: write' and must run on GitHub Actions" >&2
			  exit 1
			fi
			if [ -z "${INFISICAL_MACHINE_IDENTITY_ID:-}" ]; then
			  echo "::error::INFISICAL_MACHINE_IDENTITY_ID is empty: add the repository secret or set #identityId on #InfisicalOidc" >&2
			  exit 1
			fi
			request_url="$ACTIONS_ID_TOKEN_REQUEST_URL"
			audience='\(#audience)'
			if [ -n "$audience" ]; then
			  request_url="${request_url}&audience=$(jq -rn --arg value "$audience" '$value|@uri')"
			fi
			jwt="$(curl -fsS -H "Authorization: Bearer ${ACTIONS_ID_TOKEN_REQUEST_TOKEN}" "$request_url" | jq -r '.value // empty')"
			if [ -z "$jwt" ]; then
			  echo "::error::GitHub Actions returned no OIDC token" >&2
			  exit 1
			fi
			# A line break in a value would let it set other variables (and break
			# ::add-mask::), so refuse it without printing the value.
			has_line_break() { [ "$(printf '%s' "$1" | tr -d '\\r\\n')" != "$1" ]; }
			if has_line_break "$INFISICAL_MACHINE_IDENTITY_ID"; then
			  echo "::error::INFISICAL_MACHINE_IDENTITY_ID contains a line break; check the secret or #identityId" >&2
			  exit 1
			fi
			if has_line_break "$jwt"; then
			  echo "::error::GitHub Actions returned an OIDC token containing a line break" >&2
			  exit 1
			fi
			echo "::add-mask::${jwt}"
			echo "::add-mask::${INFISICAL_MACHINE_IDENTITY_ID}"
			delimiter="ghadelimiter_$(od -An -N16 -tx1 /dev/urandom | tr -d ' \\n')"
			{
			  printf 'INFISICAL_AUTH_JWT<<%s\\n%s\\n%s\\n' "$delimiter" "$jwt" "$delimiter"
			  printf 'INFISICAL_MACHINE_IDENTITY_ID<<%s\\n%s\\n%s\\n' "$delimiter" "$INFISICAL_MACHINE_IDENTITY_ID" "$delimiter"
			} >> "$GITHUB_ENV"
			"""
	}]
}

// #InfisicalSecretsAction fetches secrets with Infisical's official GitHub
// Action using OIDC (Pattern B: the action resolves secrets, cuenv only reads
// them from the job environment).
//
// The action logs in with the job's OIDC token, masks every secret it reads
// and exports each one as an environment variable for the rest of the job.
// Tasks read them with schema.#EnvPassthrough in their `env`. Prefer
// #InfisicalOidc with #InfisicalSecret references when you want cuenv to
// resolve only the secrets a task declares, apply environment policies, and
// redact the values itself; this contributor exposes the whole secret path to
// every later step of the job.
//
// Parameters (definition fields, so set them by unification):
// - #envSlug (required): Infisical environment slug, e.g. "prod".
// - #project (required): the Infisical project, either `{slug: "..."}` or
//   `{id: "..."}`. The action takes exactly one, so setting both fails
//   `cuenv sync ci`.
// - #identityId: machine identity ID. Defaults to the
//   INFISICAL_MACHINE_IDENTITY_ID repository secret.
// - #secretPath: folder to read (default "/").
// - #recursive: also read sub-folders (default false).
// - #domain: Infisical URL; empty keeps the action's default
//   (https://app.infisical.com). Set it for EU, dedicated or self-hosted.
// - #audience: custom `aud` claim; empty keeps GitHub's default.
// - #tasks: restrict the step to the jobs of these task IDs (default: every
//   job of the pipeline).
//
// GitHub only: other CI providers fail the step.
//
// Active when:
// - Always (add it only to projects that want it)
//
// Injects tasks:
// - cuenv:contributor:infisical.secrets-action: Runs Infisical/secrets-action
//
// Usage:
//
//	import xContributors "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [
//		xContributors.#Cuenv,
//		xContributors.#InfisicalSecretsAction & {
//			#project: slug: "my-project"
//			#envSlug: "prod"
//		},
//	]
#InfisicalSecretsAction: schema.#Contributor & {
	#identityId: string | *"${INFISICAL_MACHINE_IDENTITY_ID}"
	#envSlug!:   string
	#project!: close({slug!: string}) | close({id!: string})
	#secretPath: string | *"/"
	#recursive:  bool | *false
	#domain:     string | *""
	#audience:   string | *""
	#tasks: [...string] | *[]

	id: "infisical-secrets-action"
	when: always: true
	tasks: [{
		id:       "infisical.secrets-action"
		label:    "Fetch secrets from Infisical"
		priority: 15
		command:  "sh"
		args: ["-c", "echo 'Infisical/secrets-action is only available on GitHub Actions'; exit 1"]
		ci: github: {
			permissions: "id-token": "write"
			if len(#tasks) > 0 {
				tasks: #tasks
			}
		}
		provider: github: {
			uses: "Infisical/secrets-action@v1.0.18"
			with: {
				method:        "oidc"
				"identity-id": #identityId
				"env-slug":    #envSlug
				"secret-path": #secretPath
				if #project.slug != _|_ {
					"project-slug": #project.slug
				}
				if #project.id != _|_ {
					"project-id": #project.id
				}
				if #recursive {
					recursive: "true"
				}
				if #domain != "" {
					domain: #domain
				}
				if #audience != "" {
					"oidc-audience": #audience
				}
			}
		}
	}]
}
