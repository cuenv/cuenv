# CUE-native cross-project references (cuenv #611)

Status: accepted plan, implemented in stages (PR-A, A2, A3, S, H, 0, B1, B2 + C below).

`cuenv ci` reported "No affected tasks" when the only changed files were inputs outside the
project directory (`../content/articles/**` string inputs, or `#ProjectReference` inputs).
This spec replaces both forms with CUE-native references and fixes the silent skips found
along the way.

Decisions:

- Tasks in other projects are referenced through imports: `content.tasks.build` in
  `dependsOn`, `content.tasks.build.outputFiles` in `inputs`. No `../` inputs and no
  `#ProjectReference`.
- Callers choose the files they read; the owning project declares nothing. A path starts from
  one of three places:

  ```cue
  inputs: [
  	"src/**",                                                           // this project
  	content.#Files & {paths: ["articles/**"]},                          // another project, by import
  	schema.#ModuleFiles & {paths: ["bun.lock", "services/shared/**"]},  // the module root
  ]
  ```

The plan was reviewed over several rounds by six adversarial reviewers (CUE semantics, API
design, runtime, CI, red team, scope). Parenthesised notes such as "(red-team r13 B1)" cite
those reviews, and names such as `e10` or `m4/` are the reviewers' reproduction fixtures; the
tests listed in each section recreate them. Earlier designs that were withdrawn — named file
sets, library projects, anchoring through evaluator provenance — are mentioned only where a
rule exists because of them.

## 0. Principles (agreed)

- Identity travels in the value, stamped by the schema and exported as a regular field. No
  cuenv-specific cuengine change; cuengine stays cuenv-agnostic.
- A value carrying a non-empty foreign project is never named by bare reference metadata.
- Enrichment never fails module evaluation: it records per-node diagnostics, and those are
  raised only where a task graph, affected detection or CI compilation reaches that node
  (preserves `a_broken_reference_only_fails_the_task_that_uses_it`).
- Anything that cannot prove "unaffected" fails closed (affected plus error/warning).
- No whole `#Task` inside `#Input` (measured exponential); thin refs only.
- A path starts from one of three places, and the exported value says which: the task's own
  project (a plain string), another project (`<pkg>.#Files`, stamped with that project's name),
  or the module root (`schema.#ModuleFiles`). Owners declare nothing. A `#Base` owns no files and
  cannot be an anchor: CUE merges it into every child, so its value cannot say which directory
  it came from. Nothing is anchored by reference metadata or evaluator provenance.
- One path frame and one pattern reader. Runtime hashing, affected detection and derived
  triggers resolve every pattern through the same normaliser and the same glob test, against
  the checkout root. A pattern any of them would read differently is rejected or normalised
  before the others see it (rounds 12–13: `./x`, `{a,b}`, padded paths and `*` each split
  them). Symlinks and git submodules under plain string inputs still split them; that is
  pre-existing and filed (§8). A file reference never follows a symlink or enters a nested
  repository (§3 R5b).
- A project name means one directory, decided by value: the shallowest directory, among an
  instance's own and its same-package ancestors, whose instance carries that name. A child
  directory that inherits the name is part of that project, never a second anchor.
- `dependsOn` keeps its `#TaskNode | #ContainerImage` typing (option (i) regressed same-package
  bridge evaluation from 0.17s to about 27s and stayed exponential).

## 1. PRs

Order: **A + A2 + A3** (one release, "the A release" below; ends the two silent skips #611 reports) → **B1** (file
references; not gated) → **B2 + C** (one release). **PR-0** can start immediately and must land
before B2. **S** is its own PR in the A release. (PR-D is dropped: nothing imports another
package from its own directory any more. PR-G is moved out to its own issue, §8.)
**H** may join the A release and lands no later than B1: B1's docs teach imports and shared
references, which turns "inputs computed in another package's CUE" from rare into ordinary,
and no changed input file reveals such a change. If H slips, B1's release note states the
gap. If H files the `@embed` gap rather than closing it, H's release note names it.

- **PR-A — CI fail-closed (schema-independent).**
  1. `load_ci_projects`: build `project_map` from all discovered projects before `--path`;
     `project_configs` stays filtered (ancestor-hook behaviour unchanged; follow-up issue records
     that ancestor `propagate` hooks never run under `--path`, hook_env.rs:141-150).
     The lookup must be deterministic and mean the declaring directory (red-team r12 B1): a
     child directory in the same package inherits its parent project's `name`
     (how-to/infrastructure.mdx:395; `duplicate_project_names` exempts it,
     crates/core/src/module/unique_names.rs:49-56), and `project_map` is last-wins over a
     `HashMap` (orchestrator.rs:169-176; module_evaluation.rs:73) — on 0.72 the red team's `m4/`
     reported "No affected tasks" for `app` in 7 of 16 identical runs. Map each name to its
     declaring directory, defined by value: the shallowest directory, among an instance's own
     and its same-package ancestors, whose instance carries the same concrete `name`. Never the
     `name` field's source file: the bridge's field metadata is AST-only, has no entry for a
     name set by a definition, a comprehension or another package, points into that other
     package's directory, and lets a child that restates `name` win (runtime-engineer r13 B1,
     red-team r13 B3, cue-purist r13 B2 with six spellings). The same rule serves the loader,
     R2 and R5b. Test: 50 runs of `m4/`, all affected; the six spellings.
  2. `crates/ci/src/affected.rs`: expand groups and sequences to leaves on consumer and producer
     sides (`collect_dag_tasks`, propagation, `task_dependency_names`).
  3. Fail closed (affected + warning) on: unknown external project; malformed `#p:t`; task
     missing in external project; external `TaskIndex` failure; invalid glob
     (`crates/core/src/affected.rs`); dependency cycle. An unresolved local pipeline task is
     marked affected and then fails loudly in the executor (test asserts the failure).
  4. Un-pathed `cuenv ci` (Buildkite's generated step, a root project's GitHub thin workflow)
     skips projects with no `ci` or without the requested pipeline; with `--path` naming such a
     project it still errors. Floor: if no project in scope defines the requested pipeline, that
     is an error in both the affected and `--all` selections, listing the pipelines that exist
     (keeps ci_all.rs:161-171 passing; new test: multi-project module, mistyped pipeline).
  5. `--from <ref>` that yields no comparison is an error, distinguishing "ref not found",
     "no merge base" and "past the shallow boundary" (`provider/local.rs`).
  6. (Already on main: duplicate project names are a hard error — `duplicate_project_names`,
     crates/core/src/module/unique_names.rs. Nothing to do beyond A.1's deterministic lookup.)
  7. A string input containing a `..` path segment (which `matches_pattern` can never match)
     marks its task affected, with a warning (once per task per run): "`..` inputs cannot be
     matched against changes, so this task always runs; they will be rejected once CUE-native
     cross-project references ship (#611)". No migration advice yet (the import form
     does not exist on current schemas, and an import there mis-binds); the advice lives in PR-B2's
     R0. No new `..` support; PR-B2 rejects `..`. Implemented in `cuenv_core::matches_pattern` (a `..`-segment pattern matches when any
     file changed), so consumers, producers reached via `check_external_dependency` and
     `matched_inputs_for_task` all agree; the warning goes through `cuenv_events`; segment match only (`foo..bar/**` does not
     trigger); `matched_inputs_for_task` reports the `..` input as the selection reason. Tests:
     #611 case 2 verbatim with `--path app`; a group child with a `..` input; an un-pathed run
     selecting only the `..` task's project. An absolute string input (also never matchable
     against repo-relative changes) is treated the same way, with its own warning, also once per
     task per run. The over-run is real: on GitHub it is bounded only
     by the pipeline's derived path union, and on Buildkite, scheduled and manual runs it is
     unbounded; `deployment` tasks (cache disabled) genuinely re-run. The A release note
     says so. (Under `cuenv task` a `..` input already fails today: ci-engineer r12 N3,
     executor.rs:368-370; A.7 changes only CI's silence.)
  8. A real path frame (ci-engineer r12 B1). Changed files are worktree-relative and project
     roots absolute; `matches_pattern` reconciles them by searching for a root suffix
     (crates/core/src/affected.rs:148-170), so in a checkout whose directory is named like a
     project (`.../docs/docs`, input `docs/**`, `docs/index.md` changed) it normalises to
     `index.md` and reports "No affected tasks" (`e2`). Take the checkout root from gix, make
     every project root checkout-relative, strip it with an exact `strip_prefix`, delete the
     suffix search; canonicalise both roots before comparing (red-team r13 N7); a project whose
     root cannot be placed under the checkout is affected, with a warning. The original-path
     fallback (core/src/affected.rs:127-133) is what lets a pattern written from the
     repository root work today: project `app`, `inputs: ["shared/**"]`, no `app/shared/`, root
     `shared/x.txt` changed → `matched: true`, and `cuenv task` succeeds, so nothing ever
     flagged the pattern (`e10`). Deleting it now would turn those tasks into "No affected
     tasks" a release before `schema.#ModuleFiles` exists (ci-engineer r14 B1). A match that exists only when the pattern is read from the checkout root is one of two
     things, told apart by whether the pattern selects any file under its own project (the
     walker's first match), because affected detection alone sees only the changed file
     (ci-engineer r15 B1 `e11`, red-team r15 B1 `rootrel/`):
     - The pattern selects nothing under its project (`e10`; the message says "selects
       nothing under `app/`", which is also how a pattern whose last project file was just
       deleted reads: red-team r16 N2): the root-relative form is what the user relies on. As A.7 does for `..`: in the A release the task stays affected,
       with a warning (once per task per run) that the form is going away; from B1 the
       warning gives the rewrite (`schema.#ModuleFiles & {paths: [...]}`; when the matched file
       lies above a nested module's root there is no spelling for it, and the warning says
       so: ci-engineer r15 N3); in B2 the fallback is removed and such a match fails the run
       with the rewrite — never a silent non-match.
     - The pattern does select files under its project — the ordinary monorepo, where `app`
       has `inputs: ["package.json", "src/**"]` and the root has its own `package.json` and
       `src/`. The root match is today's false positive and the configuration is correct. It
       is never warned about as a form and never fails: the over-run stays, silently, in the
       A release; from B1 one note per run, listing the tasks and never raised as a warning
       annotation (api-designer r16 N1, ci-engineer r16 N2), says the root file also matched,
       that this stops in B2, and to add `schema.#ModuleFiles` if a task does need that file;
       in B2 it is simply not a match, and B2's release note repeats that, since the note
       shows only on runs where a root namesake changed (red-team r16 N1).
     The first-match query lives in ci/affected.rs, shared by `matched_inputs_for_task` and
     `check_external_dependency`: cuenv-core does not depend on cuenv-vcs, the walker's crate
     (runtime-engineer r16 N1). The walker counts ignored and untracked files, so a local run
     and CI can land on different sides — the warning or the note — but never on a skip.
     Item 9's parity test therefore compares files under the project. Tests: `e2`, `e10`,
     `e11`.
  9. One pattern reader (runtime-engineer r12 B3, red-team r12 B2, ci-engineer r12 B2,
     api-designer r12 B1). Today the walker and the cache treat `{` as a glob
     (crates/vcs/src/walker.rs:97-102; cache.rs:1579) and `matches_pattern` and trigger
     derivation do not (core/affected.rs:33; triggers.rs:281-283): `src/{a,b}.txt` stages two
     inputs and is never affected (`e1`, `m1/`). `./bun.lock` is hashed as an explicit file
     (walker.rs:115-125) and never passes the prefix test (affected.rs:39-42; `e8`). One
     function normalises a pattern (trim surrounding Unicode whitespace as Rust's `trim` defines it, as the walker already does at
     walker.rs:94 and the matcher does not, core/affected.rs:31-43 — `inputs: ["src/** "]`
     stages `src/a.txt` and is never affected on 0.72: cue-purist r13 B3, red-team r13 N1;
     strip a leading `./`, collapse `/./` and `//`, drop a trailing `/.`; an empty pattern or a
     lone `.` means the project directory) and one `is_glob` decides glob versus
     prefix; the walker, the cache, `matches_pattern`, `matched_inputs_for_task` and trigger
     derivation all call them. Parity test over a table of patterns (`./x`, `x/./y`, `x//y`,
     `x/.`, `"x "`, `{a,b}`, `*`, `**`, `?`, `[ab]`, a directory, a file): the set of files the walker hashes
     equals the set whose change marks the task affected. Symlinks and git submodules
     under plain string inputs are not covered here (pre-existing, filed: §8; a submodule bump
     changes one path, the gitlink, while the walker hashes the files beneath it: red-team r14
     B2, `sm/`). Release note: `./`-prefixed and padded
     string inputs now trigger their tasks; `""` and `.` mean the whole project.
  10. Derived trigger `paths` are a superset of what the runtime treats as affecting, by
     construction and not by translating one glob dialect into another (ci-engineer r12 B2 and
     r13 B2, minimalist r13 N3, red-team r12 N6). GitHub's filter syntax is not globset's: `*`
     does not cross `/`, and `?`, `+`, `[]` and a leading `!` mean something else. So for
     every input pattern `sync ci` emits only two shapes: a non-glob path as itself plus
     `<path>/**` (the existing prefix rule), and a glob as its literal directory prefix plus
     `/**` (`src/*.txt` and `src/**/*.{ts,tsx}` → `src/**`). A glob with no literal prefix
     falls back to its own anchor: `<project>/**` for a plain string, the referenced project's
     directory plus `/**` for `<pkg>.#Files`, and `**` for `schema.#ModuleFiles` (ci-engineer
     r14 B2: `schema.#ModuleFiles & {paths: ["**/*.proto"]}` needs `**`, and `content.#Files &
     {paths: ["*.md"]}` needs `content/**`). A segment holding a character special to GitHub's
     syntax — in a glob's literal prefix, in a non-glob path (`+page.svelte` and `c++` are
     emitted verbatim on 0.72: red-team r14 N4) or in a project or anchor directory — cuts the
     emitted path at its parent plus `/**`. `..` string inputs keep today's lexical resolution
     until B2 rejects them and are outside the parity test (A.7 makes them always affected). So
     are A.8's transitional checkout-root matches, which are affecting at runtime and lie
     outside every emitted prefix (`shared/**` in `app` emits `app/shared/**`): no regression
     against today, and gone in B2 (ci-engineer r16 N1). Any task the pipeline reaches — a dependency included — that
     has no inputs adds `<project>/**`: today the fallback is per pipeline (triggers.rs:94-96),
     so with `build` (inputs) depending on `gen` (none) `cuenv ci` runs `build` on a README
     change while `paths` omit the project (`e9`), as they do for a pipeline of one no-input
     task (`e4`). Workflows may start more often; `cuenv ci` still selects precisely. Parity
     test: every file whose change the runtime treats as affecting lies under an emitted
     prefix or equals an emitted path; for a task with no inputs, "affecting" means every file
     of its project. PR-B1 adds the file-reference rows, with a first-segment glob for each form. Goldens regenerate; `cuenv sync ci --check`; release
     note: "regenerate workflows; triggers are wider".
  Items 8–10 are their own PR, **PR-A3**, in the same release (minimalist r13 N6): they touch
  core, vcs, the cache and trigger derivation, and A's other items do not.
  Tests for each, including a group child and a sequence step with an external dependency.
  With A.1 (case 3) and A.7 (case 2), the A release ends #611's silent skips for users on
  current schemas without waiting for PR-0; PR-B1/B2 deliver the owner's CUE-native design.
- **PR-A2 — executor fixes for today's `#p:t` dependencies (schema-independent; same release as
  PR-A, since PR-A.1 alone turns a silent skip into a loud missing-dependency error).** §4,
  including the CI executor switching to the shared loader (the code lands here; PR-B2 §5.1
  only adds fixture tests). Own real-run test: `cuenv ci --path app` against today's
  `{project, task, map}` form. The A release note says `cuenv sync ci` still rejects
  `#p:t` dependencies until PR-B2 §5.2 (unchanged from 0.72).
- **PR-S — runs with nothing to compare run everything (own issue; ships in the A release).**
  Affected detection needs a base, and under a CI provider `HEAD^` is not one. Today, when the
  provider supplies none, cuenv compares against `HEAD^` (GitHub: crates/github/src/ci.rs:61-81
  drops a null `before`, :327-331 falls back; Buildkite: buildkite/src/provider.rs:86-90,
  112-116) and only `release` bypasses detection (orchestrator.rs:365). Reproduced on 0.72 as
  "No affected tasks": `schedule`, `workflow_dispatch`, a tag push (`when: tag` is a supported
  trigger, triggers.rs:24-27), the first push of a branch, a push whose non-null `before`
  cannot be compared, Buildkite builds started from `ui`, `api`, `trigger_job` or `schedule`,
  and a Buildkite base-branch build of one push holding two commits, which sees only the last
  (ci-engineer r12 B3 and r13 B1 `e7`; red-team r13 B1 `tagpush/` and r14 B1 `bkpush/`). The
  same symptom as #611 from a different cause. The rule (red-team r14 B1):
  - A run has a base only when the provider supplies one that can actually be compared.
    GitHub: a pull request's base; a push's non-null `before` that can be compared (in the
    checkout, or through the compare API the provider already uses for depth-2 checkouts:
    ci-engineer r15 N2, red-team r15 N3); a merge queue's `merge_group.base_sha`, which is in the payload and never read today
    (ci-engineer r14 N2), so that merge queues do not run everything. Buildkite, in its own
    terms: a pull-request build's base branch, and nothing else. It reports no previous commit
    for a branch build, and the merge base with `BUILDKITE_PIPELINE_DEFAULT_BRANCH` that 0.72
    uses for other branches (provider.rs:86-103) compares the branch with the default branch,
    not with the branch's previous tip. On 0.72 a promotion (`git push origin
    main:production`), a tag build on the default branch's tip, and a branch that merges the
    default branch in each report "No affected tasks" (red-team r16 B1, three reproductions;
    the round-16 draft had kept that merge base and was wrong to).
  - An unrecognised CI system has no base either. With `CI` set (non-empty and not `false`
    or `0`: ci-engineer r17 N1, red-team r17 N2), no provider detected and no
    `--from`, cuenv uses the local provider, which finds zero changed files in a clean
    checkout: "Context: local … Changed files: 0 … No affected tasks" (ci-engineer r16 N3
    `e7` under a GitLab-like environment; red-team r15 N4). GitLab and CircleCI are export
    targets. That run takes the whole pipeline; with `--from` it works as it does today.
    `--from` is a merge-base comparison (provider/local.rs:59-64), so on a build of the
    default branch's tip, a tag or a promotion branch `--from origin/main` finds nothing and
    `--from HEAD~1` does: how-to/ci.mdx says to pass the branch's previous tip (red-team r17
    N1).
  - Whenever a run takes the whole pipeline for want of a base, its output says so and why
    ("no comparable base (Buildkite branch build): running the whole pipeline"); users read
    the run, not the release note. Only the unrecognised-CI message mentions `--from`, which
    is ignored under a detected provider (api-designer r17 N1 and N2).
  - `HEAD^` is never guessed: both fallbacks are deleted.
  - No base → every task of the requested pipeline, through the path `release` already takes
    (not `TaskSelection::All`, which rejects matrix entries, orchestrator.rs:393-412).
  - A local run is unchanged: the local provider always has a base, and `--from` that yields
    no comparison is PR-A.5's error.
  Tests: schedule, manual dispatch, tag push, first branch push, a push with an uncomparable
  `before`, a merge-queue run (uses its base), a Buildkite `ui` build, a two-commit
  default-branch build, a promotion-branch build, a tag build and a build of a branch that
  merged the default branch in; `CI=true` with no provider, with and without `--from`; a pull
  request on either provider and an ordinary GitHub push still use detection. Release
  note: scheduled and manual runs, tag pushes, first branch pushes, deployments, every
  Buildkite build that is not a pull request, and runs on a CI system cuenv does not
  recognise (unless `--from` is given) now run the whole pipeline. Unchanged and filed
  (§8): `--from` is ignored under a detected provider (detection.rs:82-91); a child directory
  that inherits a project's name is planned as a second project, so a full run executes its
  tasks twice (`m4`, ci-engineer r13 N2); a cheaper base for Buildkite branch builds, which would have to be the branch's previous tip.
- **PR-0 — CUE fork (mandatory before PR-B2):** extend typed-reference sharing
  (docs/design/specs/cue-recursive-sharing.md) to references into imported instances.
  Acceptance: fan-out-2 project chain of depth 8 evaluates in < 1 s under the bridge; same-package
  depth-7 chain no slower than today. The fork is based on CUE 0.16.1 (cuengine go.mod); re-check
  both bars when the fork moves to a v0.18 base. If infeasible, stop and bring it back to the owner (the
  untyped-plus-hidden-check fallback fails plain `cue export` cross-package on 0.17.1).
- **PR-H — CUE-only changes fail closed (own PR; may ship in the A release).** A different,
  pre-existing silent skip from #611, with a large blast radius, so it gets its own decision and
  release note (minimalist r8).
  A change to any file in an instance's `instance_sources` (its own `.cue` files, inherited
     parent-package files and imported packages) marks every
     task of that instance affected. Today a CUE-only change reports "No affected tasks"
     (red-team r7 `grp/`, cuenv 0.72), and from B1 inputs can be computed in another package's
     CUE. Coarse, but it fails closed. Details (ci-engineer r8, red-team r8): a change to
     `cue.mod/module.cue` (or anything under `cue.mod/`) marks every task of every instance in
     that module affected — registry dependencies are not in `instance_sources`, and a schema
     bump is exactly the B1/B2 migration step; a **deleted** `.cue` file in any directory that
     contributes files to an instance marks it affected (it is no longer in the sources computed
     at HEAD); `@embed`-ed files are added to the set — if `instance_sources` cannot report them,
     that gap is filed, documented and named in PR-H's release note; `cue.mod/` and `@embed`
     files also go into the derived triggers, and both are in the parity test;
     `instance_sources` paths are module-relative and are rebased by the module root's position
     under the checkout root for affected detection (nested-module test; derived trigger paths
     stay in `sync ci`'s module-relative frame, §5.2); on the producer side, `check_external_dependency`
     treats a change to the producer instance's sources as affecting the dependency (today's
     `#p:t` has no import, so the producer's files are not in the consumer's list). The
     `instance_sources`-based CUE trigger derivation (§5.2) moves into PR-H with it, so a root
     `#Base` change also starts children's workflows. Over-run: editing the root `#Base` re-runs
     every task of every child, `deployment` tasks included, and on Buildkite (un-pathed, no
     path filters) a `cue.mod/` change re-runs every pipeline task in the module — its release
     note says so.
- **PR-B1 — caller-chosen file references (after A2; not gated on PR-0).** §2.3, `#ProjectFiles`
  and `#ModuleFiles` in `#Input`, R5b (with the exhaustive input accessor), the
  `instance_sources` extension of §4.1, the file-reference parts of §4.6 and §5, their tests and docs.
  Additive (apart from the stamping effects listed in B1's release note): `..` inputs still work (failing closed via PR-A.7) until PR-B2. `<pkg>.#Files` reads only the imported project's `name` and has no `dependsOn`
  chain, so there is no exponential cost to wait for. The cost that does exist (cue-purist r12
  N1): the bridge re-evaluates an imported project once per importer (bridge.go:736-742,
  imports.go:57) — importing a 3000-task project takes `cuenv task --path app` from 0.03 s to
  4.9 s and adds about 4 s per importer to `cuenv info`; plain `cue` shows no such cost. Stated
  in B1's release note and filed as its own follow-up (§8).
  B1 also ships node identity (§2.1 stamping: fields only, `dependsOn` typing unchanged, so no
  PR-0 dependency) plus `cuenvProject` on `#TaskOutputRef`/`#ImageOutputRef` (§2.6), the
  per-node diagnostics infrastructure of §3, R6 itself (diagnostic
  only: output refs, `ci.pipelines.*.tasks`, `#MatrixTask.task`, `#Watch.rebuild`, service and
  image dependencies, lists inspected recursively), and an interim rule covering every site R2
  covers: a `dependsOn` entry — lists inspected recursively, as in R2 — whose
  `cuenvRef.project` is non-empty and not the owner → diagnostic "cross-project task references
  arrive in vX; use `<pkg>.#Files & {paths: [...]}` for files". Local dependencies keep today's
  reference-metadata naming in B1 (R1 ships in B2), so B1 stays additive apart from the
  stamping effects listed in its release note. B1's docs teach imports, and a user's next step
  (`dependsOn: [content.tasks.build]`) would otherwise bind a local `build` by bare name
  (api-designer r8; round-4 `a7` evidence). B2 replaces the interim rule with R2's binding. Test:
  a consumer with a local `build` plus `dependsOn: [content.tasks.build]` gets the diagnostic,
  not a binding; a foreign `ci.pipelines.default.tasks` entry gets the R6 diagnostic. B1's docs
  list the residual: a foreign sequence whose members are only nested sequences carries no
  project stamp and still binds by bare name until B2's source check. B1's release note lists the
  stamping's breaking effects: `cuenvRef` becomes a reserved group child name, and re-declaring an
  imported task under the same label (`tasks: articles: content.tasks.articles`) now conflicts on
  the project stamp. The release note also announces the three forms and the must-match rule, and that `#Files`
  is now a reserved definition on every project (a project that defines its own `#Files` fails
  to export: red-team r12 N5, cue-purist r12 N3). B1 also updates the hand-written `#TaskOutputRef` literals in the docs
  (docs/design/task-output-references.md:24 and :83, reference/cue-schema.mdx) to show the new
  required `cuenvProject` field. The release note says to upgrade the cuenv binary before, or together
  with, `cue mod get`: an older binary reads the new `cuenvRef` as a group child and fails
  opaquely on any task group (red-team r10). "Lists inspected recursively" means nested sequence
  lists only, never an embedded dependency's own `dependsOn` (no duplicate diagnostics). From B1, PR-A.7's `..` warning gives R0's computed rewrite (with the `cue mod get` step first
  when the pinned schema is older than B1), never a task reference until B2. The interim and R6 diagnostics surface as a hard failure
  when CI affected detection or `cuenv sync ci` reaches the node — only for nodes the selected
  pipeline actually reaches, so an unrelated project's foreign reference never aborts other
  pipelines (§0's per-node principle; a foreign entry in pipeline `release` does not fail
  `--pipeline default` or other pipelines' `sync ci` output) — never as a skipped task. An
  un-pathed run plans every project before executing (`run_ci_projects`), so a diagnostic in a
  reached node aborts that run deliberately; the error names the project and the pipeline entry.
  CI tests, scoped likewise, include an un-pathed case.
- **PR-B2 — CUE-native cross-project task references (refs #611; gated on PR-0; ships with
  PR-C).** R1, R2's binding (replacing B1's interim rule), `outputFiles`, R0, R3–R5, the source
  check, the loader-time checks, `..` rejection, sync-ci `#p:t` support, the rest of §2–§7.
- **PR-C — remove `#ProjectReference` and leftovers.** `Input::Project`, `#p:t:path` shorthand,
  `resolve_project_reference`, `iter_project_refs` loader, `Task.task_ref`/`from_task_ref`/
  unresolved-TaskRef check, `TaskCacheConfig.project_roots`/`sandbox_project_roots`, legacy
  `#project:task` string input. Targeted Rust errors for `{project, task, map}` and
  `"#p:t:path"` (old pinned schemas still emit them). Drop the `*` default on `#TaskOutput`.
  No release tag may contain PR-B2 without PR-C.

**Docs, matrix and validation per PR** (minimalist r12 B1; CLAUDE.md: every PR updates
./docs; CI, task-execution, schema and CLI changes update
`docs/design/specs/schema-coverage-matrix.md`; `cuenv task ci.schema-docs-check` before review;
the full root flake check when the PR leaves draft).

| PR | Docs | Matrix rows | Checks beyond fmt and focused tests |
|---|---|---|---|
| A | how-to/ci.mdx: affected detection, `--from`, un-pathed runs, the `..` warning; release note | `#Pipeline`, `#Input` (notes) | `ci.schema-docs-check`; smoke `cuenv ci --dry-run` |
| A3 | how-to/ci.mdx: pattern normalisation, how trigger paths are derived; release note ("regenerate workflows") | `#Pipeline`, `#Input` (notes) | `ci.schema-docs-check`; `cuenv sync ci --check` with regenerated goldens |
| A2 | run-tasks.mdx, how-to/ci.mdx: `#p:t` execution, where `{task}` inputs are staged; release note | `#Input`, `#Task` (notes) | `ci.schema-docs-check`; real `cuenv ci --path app` |
| S | how-to/ci.mdx: runs with nothing to compare; release note | `#Pipeline` (notes) | `ci.schema-docs-check`; `cuenv sync ci --check` |
| H | how-to/ci.mdx: CUE-only changes; release note | `#Pipeline` (notes) | `ci.schema-docs-check`; `cuenv sync ci --check` (trigger paths change) |
| 0 | docs/design/specs/cue-recursive-sharing.md | none | fork tests; the two timing bars |
| B1 | §7's file-reference items; reference/schema/status.md; skill `cuenv-tasks-graph-cache` (lines 31-32 rewritten) | new `#ProjectFiles`, `#ModuleFiles`, `#AnchoredPath` (`scripts/check-schema-docs.sh:34-50` wants a row per top-level definition); updated `#Project`, `#Input`, `#Task`, `#TaskGroup`, `#Service`, `#ContainerImage`, `#TaskOutputRef`, `#ImageOutputRef` | `ci.schema-docs-check`, `ci.schema-compat`, `cuenv sync ci --check`, `cue vet ./schema` |
| B2 + C | the rest of §7; skill `cuenv-ci-release` | new `#TaskOutputFilesRef`; updated `#Input`, `#TaskOutput`, `#Pipeline`; removed `#ProjectReference` | as B1 |

A, A2, A3, S, H, B1 and B2 + C each change cross-crate CI or task-execution behaviour, and PR-0
changes cuengine's Go dependency, so each names its full-flake trigger and runs the full root
flake check once, when it leaves draft; draft commits use the focused gates above.

## 2. Schema (PR-B2; §2.1, §2.3, §2.6 and the file-reference arms of §2.4 in PR-B1)

1. **Identity.** Exported `cuenvRef: {kind: "task" | "group" | "service" | "image", project:
   _cuenvProject, name: _name}` on `#Task`, `#TaskGroup`, `#Service`, `#ContainerImage`, with
   hidden `_cuenvProject: string | *""`.
   - `#Project` stamps `_cuenvProject: name` on `tasks`, `services` and `images` entries, and on
     sequence elements via `| (#TaskSequence & [...({_cuenvProject: name, ...} | [...])])`: the
     sequence is checked as a `#TaskSequence` and its members only stamped. Spelling the members
     out as a fresh `[...((#Task | #TaskGroup) & {...}) | #TaskSequence]` in a group's child
     pattern made every task with a struct input (a file reference, `{task: "x"}`) cost minutes
     under CUE v0.17.1 and seconds under v0.18.0-rc.1 wherever it met `#TaskNode`: a pipeline
     entry, a `dependsOn` entry, a matrix entry's `task`
     (`tests/schema-compat/testdata/accept/pipeline-file-references`).
   - `#TaskGroup`: `let _groupProject = _cuenvProject`; children get
     `_cuenvProject: _groupProject`, a sequence child as `#Project` stamps one. Child-label
     regex excludes `cuenvRef`; Rust `TaskGroup` gets an explicit `cuenvRef` field ahead of the
     flattened children.
2. **Output files reference.** `#TaskOutputFilesRef: {cuenvOutputFilesRef: true, cuenvProject:
   string, cuenvTask: string}` (closed, discriminated). `#Task.outputFiles: #TaskOutputFilesRef &
   {cuenvProject: _cuenvProject, cuenvTask: _name}`. Meaning: all declared `outputs` of that
   task. Spelling: `inputs: [content.tasks.articles.outputFiles]`; same-project
   `inputs: [tasks.docs.build.outputFiles]`. Tasks only (not groups) in this PR. The name avoids
   `ci.provider.github.artifacts` and `#MatrixTask.artifacts`.
3. **File references — the caller chooses the files (owner, round 12).** Three forms, one per
   place a path can start from:
   ```cue
   inputs: [
   	"src/**",                                                // this project
   	content.#Files & {paths: ["articles/**"]},               // another project, by import
   	schema.#ModuleFiles & {paths: ["bun.lock", "services/shared/**"]}, // the module root
   ]
   ```
   - Schema (verified, scratchpad/three13):
     ```cue
     // schema/tasks.cue
     #AnchoredPath: string & !="" & !~"^/" & !~"//" & !~"(^|/)\\.\\.?(/|$)" & !~"(^|/)\\.(?i:git)(/|$)" & !~"^[\\s\\v\\pZ\\x{85}]|[\\s\\v\\pZ\\x{85}]$"
     #ProjectFiles: {
     	cuenvFiles:   "project"
     	cuenvProject: string
     	paths!: [#AnchoredPath, ...#AnchoredPath]
     }
     #ModuleFiles: {
     	cuenvFiles: "module"
     	paths!: [#AnchoredPath, ...#AnchoredPath]
     }
     // schema/core.cue, in #Project only (never in #Base)
     #Files: {#ProjectFiles, cuenvProject: name}
     ```
     Both shapes are closed and discriminated by `cuenvFiles`, so a reference that lost its
     project stamp can never be read as a module reference. At least one path, so an empty
     reference can never make a task look "has inputs, never affected". A path is a typed
     element, not a guarded comprehension (cue-purist r12 N5: clearer errors on the fork,
     0.17.1 and 0.18.0-rc.3, with no `error()` or `!= _|_`): no empty string, no leading `/`,
     no `.` or `..` segment, no `.git` segment in any letter case (a literal `.git/config` read the
     repository's metadata, and a case-insensitive file system reads `.GIT/config` too), no empty
     segment, no leading or trailing Unicode whitespace (CUE's
     `\s` alone is narrower than Rust's `trim`: a no-break space passed it, cue-purist r14 N2). So `./bun.lock`
     and `"articles/** "` — which the walker hashes and affected detection never matches —
     cannot be written (api-designer r12 B1; cue-purist r13 B3, verified on the fork, 0.17.1
     and rc.3). `.github/**`, `a..b/x`, `...`, `my file.txt` and `src/**/*.{ts,tsx}` are
     accepted. A glob can still expand to an escape the pattern cannot show (`{..,x}/y`,
     `[.][.]/y`), so Rust also checks where each resolved file lies (§3 R5b). `#Files` is a definition: nothing
     non-concrete lands in a regular field and plain `cue export` of the owning project shows
     nothing new. `name` resolves lexically to `#Project.name`. `#Files` becomes a reserved
     definition name on every project: one that already defines its own `#Files` fails to
     export (release note).
   - Language v0.18.0 changes how a base must be written (cue-purist r12 B1 and r13 B1;
     pre-existing, not caused by this design). At v0.18.0 a file-level `schema.#Base`
     embedding closes the file, the child adds `name`, and both are one package, so every
     project under an embedded base fails with `name: field not allowed` — with today's schema
     too. The open form `schema.#Base...` fixes that at v0.18.0 and is a syntax error below it
     (`postfix ... operator requires @experiment(explicitopen)`, on the fork at v0.14.1 and
     v0.16.0 and on 0.17.1). A base with no schema embed is the one spelling that is the same
     file at every language (fork v0.14.1 and v0.16.0, 0.17.1, rc.3 at v0.17.0 and v0.18.0,
     and under cuenv 0.72). Rust fixtures use it; `schema.#Base...` appears only in the
     v0.18.0 schema-compat fixture (§6); docs say which form goes with which language (§7);
     the wider docs fix is filed (§8). `root.#Files` fails in every spelling.
   - Meaning: plain path inputs (globs allowed) anchored at the named project's directory or at
     the module root. No task, no dependency edge, nothing executed or projected back, so they
     work in every workflow mode (expanded and matrix included) and under every sandbox.
   - Owners declare nothing and promise nothing. To keep that from failing silently, **every
     path or glob in a file reference must match at least one file** when a graph or affected
     detection reaches the node — not in `cuenv sync ci` (§3 R5b); a rename in the other project is a
     hard error for the caller, never a silently smaller input set.
   - `.outputFiles` stays for files a task builds (it adds the dependency edge). The #611
     reproducer's `../content/articles/**` becomes `content.#Files & {paths: ["articles/**"]}`.
   - Module-root files (`bun.lock`, `Cargo.lock`, `flake.lock`) and files beside a `#Base` that
     several projects share use `schema.#ModuleFiles` with module-relative paths. A `#Base` (root
     or nested) has no `#Files`: `root.#Files` / `services.#Files` → `undefined field: #Files`.
     (With `#Files` on `#Base`, the root's and a nested base's references exported identically,
     so `services.#Files & {paths: ["shared/**"]}` would have been read from the module root:
     scratchpad/callers. Anchoring bases through the fork's `provenance.Analyze` was rejected by
     the owner: the anchor would not be in the value, it needs a fork change, and it runs with
     structure sharing off.)
   - An unqualified `#Files` is not in scope in a project's own file (it comes from the embedded
     `schema.#Project`); a project's own files are plain strings.
   - Verified with plain `cue` (dev build, language v0.17.1, scratchpad/three): `cue vet
     ./schema`; full exports of the root base, `content`, `app`, a nested base and a project
     under it; the reference survives a `let`, a comprehension, a hand-written literal and a
     re-export through a non-project package (`cuenvProject: "content"` in each). Rejected: `..`
     segment, leading `/`, typo `pths`, empty `paths`, missing `paths`, a spoofed `cuenvProject`
     on an imported reference, `cuenvProject` on `#ModuleFiles`, a reference in `dependsOn`,
     `#Files` from a base and from a non-project package; `./x`, `.`, `a/./b`, `a//b` and `""`. cuenv 0.72 (the fork's evaluator) evaluates the module: `env
     print` shows the inherited base, and the only failure is Rust not knowing the new input
     shape. Re-run by cue-purist r12 on the fork and on CUE v0.18.0-rc.3 at language v0.18.0:
     the three forms, closedness, the spoof, and survival through `let`, comprehensions,
     `list.Concat`, embedding and re-exported lists and maps. Not verified: v0.18 final, any Rust.
4. **`#Input` (until PR-C):** `string | #ProjectReference | *#TaskOutput | #TaskOutputFilesRef |
   #ProjectFiles | #ModuleFiles`.
   No error arm of any kind: a named always-⊥ definition fails `cue vet ./schema`
   and, like an inline catch-all, replaces every malformed input's specific error (typos in
   `{task}`) with one generic message (api-designer r3, cue-purist r3). Instead,
   cuenv's CUE-error renderer (cuenv side, not cuengine) appends "a task is not an input: write
   <task>.outputFiles to consume its declared outputs" when an error path matches
   exactly `<tasks|images>.….inputs.<n>.(cuenvRef|outputFiles): field not allowed` (not
   anchored on `tasks.`; pinned by a test, re-checked on v0.18). A group used as an input fails
   at `inputs.<n>.<child>.cuenvRef`; it gets a group-specific hint ("groups cannot be inputs yet;
   list the children's `outputFiles`"), never the task hint. Hints are keyed on the error text of the fork's evaluator, the only one cuenv shows
   (cue-purist r12 N2: 0.18.0-rc.3 words several of these lines differently, and under the
   bridge an undefined `#Files` reads `#Files: undefined in package`, imports.go:63), and each
   is pinned by a test that fails when the text stops matching. No new CUE-text hints ship for
   file references in B1 (minimalist r12 N6): the cases that would be silent or opaque are
   Rust diagnostics (§3 R5b), and the rest are a follow-up with their real error lines
   recorded (api-designer r12 N1; §8). One exception, keyed on a field path and not on message
   text, because the typed path's raw error is a regular expression (api-designer r13 N1):
   `inputs.<n>.paths.<m>: invalid value`, matched on the raw CUE text before cuenv's renderer
   wraps the line (cue-purist r14 N4) → "paths are relative to the project (for
   `schema.#ModuleFiles`, the module root): no `..`, `.`, `.git` or empty segment, no leading `/`, no
   surrounding space; for files outside the project use `schema.#ModuleFiles`".
   Acceptance: `cue vet ./schema` exits 0.
5. **`..` rejection in the schema.** A hidden guarded field on `#Task` and `#ContainerImage`:
   `_cuenvInputCheck: {if inputs != _|_ for v in inputs if (v & string) != _|_ if
   strings.Contains("/"+(v & string)+"/", "/../") {(v & string): error("...")}}` (`v & string` in the label and inside the
   call: cue-purist r12 N4 and r13 N4 — a `*ref | "str"` input otherwise trips the check; `tasks.cue` imports `strings` for it) — the form verified in
   cue-purist/x6/schema.final minus its absolute-path clause (unguarded forms break `dependsOn`
   on a group containing a sequence; the error is keyed on the offending string). It unifies the
   error with the user's value — `(v & string): v & error(...)` — so the user's file:line appears
   (api-designer v4c, cue-purist x8/schema.v5); pinned by a test and included in the v0.18
   re-run. Message: "input <v> leaves the project directory: import the project that holds these
   files and use `<pkg>.#Files & {paths: [...]}` (or `<pkg>.tasks.<name>.outputFiles` for built
   outputs); for files outside every project use `schema.#ModuleFiles & {paths: [...]}` — see
   <docs anchor>". (File references carry their own path check, §2.3.) On a current schema this
   CUE error comes before Rust's R0, so the renderer appends R0's computed rewrite to it, keyed
   on the field name `_cuenvInputCheck."<v>"` and not on message text (api-designer r12 N4).
   `error()` is already used in schema/infrastructure.cue, so the pinned fork supports it.
   Absolute string inputs keep today's behaviour schema-wise (PR-A.7 makes them fail closed in
   affected detection).
6. `#TaskOutputRef` and `#ImageOutputRef` gain `cuenvProject`.
7. `dependsOn` typing unchanged (relies on PR-0 for cross-package performance).

## 3. Identity rules (Rust, PR-B2; the diagnostics infrastructure, R5b, R6 and the interim rule in PR-B1)

Run in the JSON enrich pass in `ModuleEvaluation::from_raw_parts`, before `process_output_refs`,
using the instance's own `name`. Violations become per-node diagnostics (a poisoned `_name` or
an `Instance.reference_errors` entry), never a failed evaluation.

- R0 a string input with a `..` segment, regardless of the pinned schema version (old schemas
  still pass `../` through) → diagnostic with the import message.
- R1 `cuenvRef.project` == owner → local; name = `cuenvRef.name`, or reference metadata when
  `name` is empty (own-instance sequence item; the bridge fills it). The name alone does not
  make a stamp local: where the evaluation records declarations, the stamp's `project` must be
  declared where the instance's own `name` is. A same-named project of another package (one
  vendored under `cue.mod/pkg`, say) is foreign and takes R2, so binding through the imports
  fails it with the out-of-module error; a stamp whose declaration was not recorded, while the
  instance's was, is foreign too. The same test decides an `outputFiles` input (R5), an output
  reference, and every entry R6 inspects.
- R2 `cuenvRef.project` non-empty and ≠ owner → `_name = "#p:name"`. Lists in `dependsOn` (whole
  sequences) are inspected recursively before any metadata naming; any member with a foreign
  project, or a foreign entry with empty `name` → diagnostic "depend on a named task or group of
  project p". Nested-only foreign sequences fall to the source check; the residual
  undetectable case (an empty foreign sequence) is documented.
- R3 `cuenvRef` present with empty project → allowed only when reference metadata resolves to a
  local path (keeps the tested `#Svc.tasks.migrate` → local `migrate` pattern); otherwise
  diagnostic "tasks outside a project cannot be referenced; move them into a project".
- R3b `cuenvRef` absent (old pinned schema) → legacy metadata naming, then the source check below.
- **Source check** (whenever a node is named by reference metadata and carries no non-empty
  `cuenvRef.project`: R1's empty-name branch, R3, R3b). Two halves, because embedded values are
  discarded by `TaskIndex::build` (index.rs:230-234) before the loader runs:
  - *Record* (enrich time, inside the task-refs pass, before any replacement): for each such
    dependency, look up `metadata.sources` by the original field path — the same lookup as
    `executable_body_source`, `<dep path>[i]….command|script` — so whole-sequence dependencies
    (which the refs pass replaces with `{"_name": …}`) are covered and pass order does not
    matter. Collect the dependency node itself (for a group, its children paired by path),
    excluding nested `dependsOn` and `inputs` subtrees, and the bound local node's sources the
    same way → `Instance.dependency_sources`. Fixture: `content.tasks.nest: [[a], b]` vs a local
    `nest`.
  - *Compare* (loader / graph build, when imported instances are known): a recorded `_source`
    file is foreign when it belongs to an imported **project** instance's own files and not to
    the owner instance's own files. "Own files" = the instance directory plus same-package files
    in parent directories, not the packages it imports; the loader's per-instance records (§4.1) say which
    files belong to which imported instance; filter to project instances (a non-project package
    must never count as a project) — no directory-containment arithmetic, so a non-project
    package under a root project is not misclassified. For a sequence or group, compare the set of leaf
    task sources. If the bare name bound a local node, flag only when that node's `_source`
    differs (keeps tasks built from a shared root or library template local). If it bound
    nothing, enrich the missing-task error with the same hint, worded "defined outside this
    project" (never naming a guessed project). Project instance files are fetched on demand
    (`instance_sources`) at graph build when no `#p:t` dependency already loaded them. Message: dependency path,
    file:line, and — only when the pinned schema is older than vX — "this is another project's
    task: cross-project references need cuenv schema ≥ vX; run `cue mod get
    github.com/cuenv/cuenv@vX`". On a current schema (the R3 branch) the message is "defined in
    project p's directory: reference `p.tasks.<name>`".
  - Deferrable: it is a migration aid for old pinned schemas (`schema_compat.rs` already warns
    on version skew); first item to move to a follow-up if PR-B2 grows. **Deferred in B2**:
    the release note says to upgrade the schema before depending on imported tasks (§7);
    filed in §8.
  - Best-effort and documented: `_source` is the `command`/`script` position, so two foreign
    and local tasks built from the same template remain indistinguishable on old schemas; groups
    carry no `_source`. Value equality was rejected (red-team r3 table: misses identical bodies,
    flags `_svc.tasks.migrate`). Tests: every row of that table plus a template-built producer.
- R4 a non-empty `cuenvRef.project` is authoritative.
- R5 `#TaskOutputFilesRef` input → rewritten to `{"task": "<local or #p:t>"}` (existing
  `#TaskOutput` shape; no new `Input` variant). Empty `cuenvTask` → diagnostic.
- R5b file-reference inputs → path inputs with no dependency edge. `#ProjectFiles` is anchored at
  the directory that declares the project named by `cuenvProject`; `#ModuleFiles` at the CUE
  module root.
  - **Shape checks before typed deserialization** (cue-purist r12 B2). The bridge exports
    leniently (crates/cuengine/values.go:40; module_evaluation.rs:118), so what `cue export`
    rejects can reach Rust: `inputs: [content.#Files]` arrives as `{cuenvFiles: "project",
    cuenvProject: "content", paths: null}`, and today that fails the whole project with `data
    did not match any variant of untagged enum TaskNode`. In the JSON enrich pass a `null`
    input, a file reference with null, absent or empty `paths`, a non-string path, or a null or
    empty `cuenvProject` becomes a per-node diagnostic naming the task and its file:line ("say
    which files: `<pkg>.#Files & {paths: [...]}`", or `schema.#ModuleFiles & {paths: [...]}`
    when `cuenvFiles` is "module"), and the node is left well-formed so the rest of the project
    loads. The list covers everything the lenient export emits (cue-purist r13 N2, 19 shapes).
  - **Type.** `Input::Files { anchor: Project(name) | Module, paths }` with a hand-written
    `Deserialize` keyed on `cuenvFiles` (`Input` is `#[serde(untagged)]`, inputs.rs:31, and an
    untagged enum cannot be keyed on a field).
  - **No reader can drop it** (runtime-engineer r12 B1). Every reader outside the cache goes
    through `iter_path_inputs`, a `filter_map` that silently skips any other variant
    (crates/manifest/src/tasks/task.rs:406; used at triggers.rs:213, ci/affected.rs:216,
    lowering.rs:266, core/tasks/affected.rs:14 and :28). Replace those helpers with one
    exhaustive accessor (a `match` with no wildcard arm), so the compiler makes each reader
    decide. An unbound reference is then: affected plus a diagnostic in affected detection
    (`AffectedBy` returns `bool`, core/tasks/affected.rs:13, so its caller collects the
    diagnostic, §5.1); a compile error in trigger derivation and IR lowering
    (`collect_inputs_from_node` returns `()` today, triggers.rs:210, so a `Result` is threaded
    through it and its callers); an executor error raised before the cache branch
    (executor.rs:241-244), which covers `hermetic: false` too — not in cache.rs, which degrades
    to a skip (cache.rs:182, 235) and returns before inputs for `hermetic: false` (cache.rs:197). Commands that bind:
    `cuenv task`, `cuenv ci` (real, `--dry-run`, `--export`), `cuenv up` and `cuenv sync ci`.
    Listing and completion commands deserialize it and never read it. `cuenv build` does not
    read `#ContainerImage.inputs` today (build.rs), so it has nothing to bind.
  - **Binding** (red-team r12 B1/B3 and r13 B3, runtime-engineer r12 B2 and r13 B1, cue-purist
    r13 B2). The loader (§4.1) returns one record per instance the consumer imports: import
    path, directory, package, module, and its `name`. A project name N resolves to N's
    declaring directory by PR-A.1's rule, by value: the shallowest directory, among the
    imported instance's own and its same-package ancestors, whose instance carries the same
    concrete `name` (defaults resolved). The ancestors are the directories of the instance's
    own files; a directory in between that holds another package, or no file of this one, is
    skipped, not an end of the walk and not an error (cue-purist r14 N1, runtime-engineer r14). The consumer's own name resolves the same way from its own directory.
    Diagnostics, each decidable from those records, raised when a reference to N is bound:
    no imported instance carries N → "import the project", printing the import line
    (`content "<module>/content:cuenv"`); an imported instance carrying N is not N's declaring
    directory (`content/drafts`, alone or beside `content`) → "import the project's own
    directory `content`", naming the package that imports it, since the import may be
    transitive (the two export identically: red-team `a1-child/`); N's instance is in
    a package other than the one cuenv evaluates → "`<dir>` declares package `<p>`; cuenv
    projects use package `<pkg>`"; N's instance belongs to another module (decided by the
    instance's module path, not by directory containment, so a vendored copy under the module
    root is not mistaken for a local project: red-team r13 N3) → "same CUE module only"; two
    instances with different declaring directories carry N → the duplicate-name error. Names
    are read for out-of-module candidates too, so a same-named local project is never bound in
    their place (red-team `xmod/`). Binding reads only `name`, by field lookup on the built
    value, so a project referenced only through `#Files` cannot fail the caller on its own
    unrelated errors — a guarantee about errors, not cost: the import's package is still
    built (cue-purist r14 N3) (tests: a broken task in `content`; a
    broken third project). `#ModuleFiles` needs no loading: its anchor is the nearest `cue.mod`
    above the consumer, from one helper shared by `cuenv task`, CI (which today takes the
    module root from the working directory, ci/discovery.rs:64) and trigger derivation.
  - **Path checks in Rust**, the same rules as `#AnchoredPath`, on every bound path whatever
    its origin: `Path::join` with an absolute path replaces the anchor, and an old pinned
    schema or hand-written JSON never saw the schema check. And on every result: each file a
    reference resolves to must lie under its anchor, because a glob can expand to an escape
    that no pattern check sees (`{..,x}/y`, `[.][.]/y`: red-team r13 N2, cue-purist r13 N3).
  - **Symlinks and submodules: a file reference stays in real directories of its own
    repository** (red-team r12 B2, r13 B2 and r14 B2; runtime-engineer r13 B2; minimalist r14
    N5). Today the walker follows links (walker.rs:143-158) and canonicalises a glob's base
    directory and explicit paths before it walks (walker.rs:136-166), while affected
    detection matches a changed file's real path: a reference through `vendor -> ../shared`
    would re-run locally and never in CI, `up -> ..` stages the whole repository, and
    `current.txt -> src/a.txt` is staged but never affected (`m3/`, `m5/`, `ws/`). A git
    submodule splits the same way: its files are hashed, and a bump changes one path, the
    gitlink (`sm/`: `themes/**` stages two inputs and is skipped after the bump). Two rules,
    passed to the walker as an options struct; neither needs the git index:
    1. During a reference's walk, a symlink (to a file or to a directory) and a nested
       repository (a directory other than the checkout root that holds a `.git` file or
       directory: a submodule, a worktree or an embedded checkout) are not entered and not
       included, and are not an error; nor is the repository's own `.git` directory (on 0.72
       a `**` at a root project stages 100 inputs, 65 of them from `.git`: red-team r15 N2). A glob therefore selects regular
       files under real directories of this repository — the same set locally and in a clean
       checkout, whatever untracked links exist (`node_modules/docs -> ../docs`, Nix's
       `result`, `.venv/bin/python`). A retargeted link or a bumped submodule is a changed
       path that may still match the pattern in CI: an over-run, never a skip.
    2. A pattern's literal prefix, or an explicit path, that passes through or ends at a
       `.git` segment (any letter case, whether or not it exists), a symlink or a nested
       repository → diagnostic naming it ("reference the target
       directly"; "a file reference cannot reach into a submodule"). Each component is
       checked from the checkout root down (outside a git repository, from the hasher root:
       runtime-engineer r15 N2) — never above it, so a symlinked home directory or `/var`
       cannot trip it (runtime-engineer r14 N2) — with `symlink_metadata`, before
       anything is resolved. No walk is needed, so every binding command reports it,
       `cuenv ci` included.
    Nothing is hashed that CI cannot see, and no link needs a representation in the input
    tree (which has none today: vcs/hasher.rs:10-21, cas/merkle.rs:62). The cost, documented:
    a glob silently leaves out linked files and submodule contents — name the target; a
    submodule's own files are out of reach. A sandboxed task that reads a left-out link fails
    on a missing file, loudly; a task run in the live checkout (`cuenv ci`, runner.rs:138-141,
    and `hermetic: false`) can still read a link's target or a submodule's files, which are
    then not among its inputs, as with any undeclared read (runtime-engineer r15 N3, red-team
    r15 N1). Tests: `vendor/**` and `vendor/x.txt` through a
    directory link; `**` over untracked links; an explicit path that is a file link; a glob
    over a file link (skipped); `themes/**` into a submodule (diagnostic) and `**` over one
    (skipped); the same under `cuenv ci --dry-run`.
  - **Must match.** Each path or glob must resolve to at least one file, where "resolve" is
    the walker's own resolution of the normalised pattern (PR-A.9) under the anchor, so the
    check and the hash cannot disagree: it reads the working tree, ignores `.gitignore`, and
    counts untracked and ignored files. Early exit on the first match; memoised per (anchor,
    pattern) within a run. Otherwise a diagnostic naming the caller task and its file:line,
    the anchor directory and the pattern, and saying what to do ("`articles/**` matches nothing
    in content/ — fix or remove the path; files a task builds come from `<task>.outputFiles`").
    Applied wherever a reference is bound for hashing or for affected detection, and **not** in
    `cuenv sync ci`, which only derives triggers and must not depend on a working tree. In
    `cuenv ci` references are bound before the selection branch, so `--all` and release runs
    are checked too, and over the pipeline's reachable closure, not during short-circuiting
    propagation (ci/src/affected.rs:107-109, 138-147). A reference cannot be optional: a glob
    that is legitimately empty at times (`.cuenv/changesets/*.md` after a release) is a hard
    error for its callers — documented, with optional references as a follow-up (§8). Tests:
    a rename in the other project; a glob with one match; a directory path; an untracked file
    (matches); an ignored file (matches); `--all`.
  - A reference may be inherited from a base or built in a shared package; that is harmless,
    because the anchor is in the value (cue-purist r12 N7).
  - Expanded in §4.6's frame into module-relative `MappedInput`s: `source = anchor_path/p`,
    `destination = anchor_path/literal_prefix(p)` (`anchor_path` empty for the module root),
    `producer_task: None`; excluded from consumer prefixing, rebased onto the hasher root (the
    git root, or the module or project root where a command passes none: `cuenv up`,
    up.rs:405, executor.rs:184-189), merged when identical. `cuenv ci` stages nothing
    (runner.rs:139), so a reference relies on "staged path equals checkout path"; §4.6 keeps
    that true.
  - CI affected detection (§5.1) and trigger paths (§5.2) read the same bound paths through
    PR-A.8's frame and PR-A.9's reader.
  - Never lowered to a consumer-relative string (which would trip PR-A.7 and R0).
- R6 foreign identity in output refs, `ci.pipelines.*.tasks`, `#MatrixTask.task`,
  `#Watch.rebuild`, services and images → diagnostic "not supported across projects yet"
  (list entries inspected recursively, as in R2).
- Loader-time checks (§4): project names resolve **only among the consumer's imported project
  instances** (transitively, via `instance_sources`), never by scanning all projects — so a
  hand-written `cuenvRef` naming a project that is not imported fails as unresolved, and one
  naming an imported project binds to that project's real node (no capability beyond the
  import; `cuenvRef` documented as reserved; no `_source`-based spoof check, which would reject
  template-built producers). Coexistence until PR-C: a `#p:t` implied by an `Input::Project` on
  the same task keeps today's name/path resolution; every other `#p:t` resolves only among
  imported projects. A producer used via `outputFiles` declares `outputs`; cross-module
  imports → "same CUE module only". Project names are unique (already on main,
  unique_names.rs) and resolve to their declaring directory (PR-A.1); the package and module
  diagnostics are R5b's binding rule, decided from the loader's per-instance record. `cuenv ci`
  discovers `package cuenv` only and ignores `--package` (discovery.rs:59): unchanged, filed (§8).
- R0's message is precise: when the resolved path lies in a known project p, it names p, its
  import line and the rewritten reference (`p.#Files & {paths: ["<rest of the path>"]}`) —
  unless p already imports the consumer, where that import would be a cycle that breaks every
  importer of either project (api-designer r13 N4). In that case, and when the path lies in
  no project, it gives the module form with the module-relative path (`schema.#ModuleFiles &
  {paths: ["<path>"]}`). When the pinned schema is older than vX, the `cue mod get` step comes
  first (`#Files`/`#ModuleFiles`/`outputFiles` do not exist on old schemas).
- R6's message for a foreign `.stdout`/`.stderr`/`.exitCode` names the alternatives (pass
  files via `.outputFiles`, order via `dependsOn`).
- `TaskDependency._rest` (the embedded value) is dropped after these checks.

## 4. Execution (PR-A2; used by `cuenv task`, `cuenv ci`, `cuenv up`)

1. Shared cross-project loader in `cuenv-task-exec` or `cuenv-core`, replacing the CLI-only
   `extend_task_scope_with_cross_project`. `cuenv task` evaluates one path-local instance
   (implementation.rs:42, executor.rs:61, module_evaluation.rs:113); its only route to other
   projects today is `discover_all_modules` (executor.rs:121-147), which evaluates the whole
   module and fails if any directory fails (runtime-engineer r12 B2). The loader instead:
   - asks `cuengine::instance_sources` (bridge.rs:1039) for the consumer's imports. Today it
     returns bare file paths and drops files outside the module (sources.go:21-28, 100-108),
     so "wrong package", "another module" and "not imported" look the same (red-team r12 B3).
     It is extended — generically, nothing cuenv-specific — to return one record per imported
     instance: import path, directory, package name, module path, and the values of the field
     paths the caller asks for (cuenv asks for `name`), read by field lookup on the instance's
     built value and, for the same paths, on its same-package ancestor directories (the
     directories of its own files; one in between that holds another package or no file of
     this one counts as absent; loads are memoised per directory within a run and are part of
     B1's measured import cost: runtime-engineer r14 N3);
   - does **not** evaluate a candidate on its own. `export_paths` only shrinks the export:
     loading, building and failure reporting are unchanged (bridge.rs:265-268) and the bridge
     drops an instance whose root is in error (bridge.go:743-747), so `cuenv env print --path
     content` fails on one broken task while `cuenv task --path app`, which imports `content`,
     works today because the import is evaluated lazily (cue-purist r13 B2, `decl/broken`).
     A field lookup works when the root is in error (`name="content" rootErr=true`);
   - computes each name's declaring directory from those values by PR-A.1's rule;
   - loads `#p:t` targets transitively, and only then evaluates a project's tasks. A project
     referenced only through `#Files` stops at the step above.
   PR-A2's legacy `{project, task}` form has no import to follow, so for it A2 keeps today's
   discovery (with A.1's name rule); the import records arrive with B1 (ci-engineer r13 N7).
   (Round 12: the (directory, package) keying, the per-package evaluation and the hook,
   runtime and lockfile caveats of rounds 6–10 existed only for library projects and are
   removed.) `cuenv up` uses the loader too. Test: an unrelated directory that fails to
   evaluate does not fail `cuenv task --path app`.
2. External tasks live in a graph-only scope (not selectable by name or label).
3. One execution context per loaded project: root, config, hook environment (CI), runtime env,
   tool downloads, activation steps, `TaskSelection`, runtime cache identity. The environment key
   includes the project; contexts match the unqualified task name; `-e` applies to every project
   (a project without that environment uses its base). Tests for `cuenv task` and `cuenv ci`
   with a hook-provided variable: the producer sees its own, never the consumer's secrets.
4. Qualify external output-ref placeholders, `output_ref_deps` and `Input::Mapped.producer_task`.
5. Expand task-output inputs after loading (second pass keyed by qualified name).
6. Paths — one frame. `MappedInput.source` and `.destination` are both **module-relative**.
   A `{task}` input without `map` (same- or cross-project) uses the producer's output path as
   its destination (staged where it lives; unchanged when producer and consumer share a
   directory); an explicit `map` resolves `to` as consumer project path + `to`, so expansion
   (`expand_cross_project_references`) takes the instance path as a parameter.
   `resolve_mapped_input` rebases both by the module root's position under the hasher root — the
   git root, or the module root where a command passes none (runtime-engineer r12 N1) — which
   fixes the nested-module bug. `cuenv up` passes the task hasher root as `cuenv task` does
   (implementation.rs:690); where a command's hasher root does not contain an input's anchor
   (a project-root fallback, executor.rs:184-189) the input cannot be rebased, and that is a
   diagnostic, never a silently dropped input (runtime-engineer r13 N1). Mapped entries are excluded from `place_project_inputs`'
   consumer-prefixing. Identical entries (path, digest, mode) merge in
   `InputDirectoryBuilder::insert` instead of colliding; `execution.inputs` is deduplicated
   before `exec_root::prepare` (its `std::fs::copy` keeps 0444, so re-staging would fail).
   `map` under `sandbox: "none"` or `hermetic: false` (never materialised, yet keyed) → a
   diagnostic. Tests with module root ≠ git root for same-project `{task}`, same-project
   `{task, map}` and cross-project `outputFiles`. Release note: `{task}` without `map` now stages
   at the producer's path (differs only when the producer's `dir` is not the consumer's).
7. Contributors: test that a producer project's workspace contributor is not injected twice.
## 5. CI (PR-B2; file-reference matching and triggers in PR-B1)

1. CI affected detection resolves `#p:t` and file references through the shared loader. One CI
   project set serves affected detection and execution. Every pattern is matched in PR-A.8's
   frame — checkout-relative, exact prefix — through PR-A.9's reader. A file reference's
   patterns are its anchor's checkout-relative path (the project's declaring directory, or the
   module root) joined with each path, matched after the normaliser with no project prefix added. References are bound, and must-match
   applied, before the selection branch and over the reachable closure (§3 R5b); the
   diagnostics are collected by the caller of `AffectedBy` and fail only the pipelines that
   reach the node. `--dry-run`, `--export` and `cuenv sync ci` bind references too (anchoring
   needs the project's directory); `sync ci` does not apply must-match. Transitive anchors: a
   foreign producer's file references keep their own anchor, never the producer's or the
   consumer's (test: `app` depends on `#content:build`, whose inputs include `schema.#ModuleFiles
   & {paths: ["bun.lock"]}` and `docs.#Files & {paths: ["api/**"]}`; only `bun.lock` changes →
   `build` and `app` affected; only `docs/api/x.md` changes → the same).
   CI executor uses §4.1; integration tests: `--path app` real run, un-pathed real run.
2. `cuenv sync ci`: IR lowering/validation accepts `#p:t` dependencies in single-job workflows.
   Derived `paths` come from the loaded producer `Project` (groups and transitive `#p:t` walked
   as at runtime): producer inputs, `<producer>/**` when the producer has no
   inputs, and file-reference paths (anchored at the project or the module root), all in `sync ci`'s one frame (below) and in PR-A.10's two shapes. CUE trigger paths (moved into PR-H) come from
   the consumer's `instance_sources` (transitive imports included), replacing "producer
   `env.cue`", collapsed to directories
   (`<dir>/*.cue`) and keeping `cue.mod/**`, so adding a `.cue` file does not churn every
   consumer's workflow. The parity test adds file-reference cases: a `#ModuleFiles` non-glob path
   (`Cargo.lock`) from a nested consumer and a `#Files` glob into a sibling project. A parity test
   asserts every path the runtime treats as affecting is covered by the derived `paths`, with a
   group producer and a transitive `#p:t` producer.
   One frame per list (ci-engineer r12 B4; "enough", r13): `sync ci` is module-rooted today
   (sync/functions/implementation.rs:146-152; github/implementation.rs:86) and writes
   module-relative paths; file-reference paths are emitted in that same frame, so a list never
   mixes two. With module root ≠ checkout root the generated workflow is already unusable
   today (it is written under the module, `e3`): a pre-existing bug, filed (§8). The
   nested-module cases are therefore out of the `sync ci` parity test until it is fixed; they
   stay in the runtime and affected-detection tests.
3. Any emission of per-task `--skip-dependencies` jobs (expanded standard, expanded release,
   matrix in either mode) whose lowered IR task set (after contributors are applied, including
   dependencies of pipeline tasks) contains a `#p:t` dependency → compile error
   naming the dependency (and `mode: "thin"` where that applies). Not raised by
   `cuenv ci --export`, which always emits single-job steps. Thin+matrix test. File references create
   no dependency, so lockfile inputs via `schema.#ModuleFiles` and sibling files via
   `<pkg>.#Files` work in expanded and matrix workflows (golden tests for both).
4. Golden tests: `cuenv sync ci --check`, `cuenv ci --export buildkite` on the fixture.
5. "No inputs → always affected" counts task-output and file-reference inputs, matched precisely now
   that they are resolved (moved here from PR-A; file references in PR-B1, task outputs in PR-B2).

## 6. Tests (PR-B1 for file references, PR-B2 for the rest)

Integration fixture, the #611 reproducer rewritten (languages: end of this section): `content` (an
`articles/` directory it declares nothing about, plus a real producer task with `outputs`) and
`app` importing it — `inputs: [content.#Files & {paths: ["articles/**"]}]`; `dependsOn:
[content.tasks.build]`; `inputs: [content.tasks.build.outputFiles]`; a root `#Base` with
`bun.lock` beside it, read by `app` as `schema.#ModuleFiles & {paths: ["bun.lock"]}`; a nested
`#Base` (`services/`) with a project under it reading `schema.#ModuleFiles & {paths:
["services/shared/**"]}`; `services.#Files` and `root.#Files` (rejected: `#Files: undefined in package` under the bridge);
children still inherit both bases; a reference through a `let`, a comprehension and a re-export
from a non-project package; a path that matches nothing (hard error); a rename in `content`
(hard error in `app`); a reference to a project that is not imported; `..` and a leading `/` in
`paths` (schema and Rust); file references in thin, expanded and matrix workflows; a local task and a local sequence also named like the
foreign ones (no mis-binding); `let` alias; group dependency; nested group; dependency on a
group containing a sequence; image and service dependencies (same project OK, foreign
rejected); foreign sequence item (rejected); `..` string (schema message, and R0 with an old
schema); whole task in `inputs` (renderer hint); typos in `{task}` (their specific CUE error,
no hint); hand-written `cuenvRef` naming a
non-imported project (unresolved; in `cuenv ci` it is selected as affected and then fails
loudly); a task with both a `{project, task, map}` input and an imported `dependsOn` on the same
project (both resolve to one node); old pinned schema: foreign dependency bound and unbound
(source-check message with upgrade step), template-built producer; producer env isolation;
contributors. The depth-8 perf chain is generated in a temp directory inside the test (plain `cue export`
of it exceeds 60 s on stock CUE, so it must never be a checked-in fixture); only the depth-3
chain is checked in, where `ci.schema-compat` exports it. Commands: `cuenv task`,
`cuenv ci --dry-run`, `cuenv ci` (real, `--path` and un-pathed), `cuenv sync ci --check`,
`cuenv ci --export buildkite`. Perf: after PR-0, depth-8 fan-out-2 chain < 1 s under the bridge;
plain `cue export` of the fixture and a depth-3 chain succeeds.

Languages (cue-purist r12 B1 and r13 B1). The fork is on a CUE 0.16.1 base and refuses a
module that declares `v0.18.0` ("too new for current language version v0.16.1"); every
existing bridge and CLI fixture declares v0.14.x or v0.16.0. So:
- Rust integration fixtures declare a language the bridge loads and write each base with no
  schema embed — the one spelling that is the same file at every language, so the fixtures
  move with the fork unchanged. `schema.#Base...` is a syntax error below v0.18.0.
- `ci.schema-compat` gains a multi-directory consumer fixture at language `v0.18.0`, run with
  stock CUE v0.18: a base written `schema.#Base...`, a nested base, two projects, one import
  and the three forms, with an embedded `schema.#Base` as a reject case. That is where §7's
  "re-run on v0.18" actually runs: today the script copies one directory per consumer
  (`scripts/check-schema-compat.sh:149`) and runs importing fixtures at v0.14.1 only (:203).

Added in round 13: PR-A's `m4/` (50 runs), `e2` and the pattern parity table; PR-S's two
events; a reference with `null` paths under the bridge (per-node diagnostic, the rest of the
project loads); a child-directory import (`content/drafts`); a same-named project in another
package and in another module; a symlinked directory and an escaping symlink under a
reference; `./x` and `{a,b}` in string inputs.

Added in round 14: the six spellings of a project's `name` (declaring directory by value);
importing `content` and `content/drafts` together; a vendored same-named project; PR-S's tag
push, first branch push and Buildkite `ui` build; a padded path in a string input and in a
reference; `{..,x}/y` (resolved-file containment); the symlink cases of R5b; `e9` (a no-input
dependency) and the two-shape trigger goldens.

Added in round 15: `e10` (a root-relative string pattern stays affected, with a warning, until
B2); first-segment globs in `#Files` and `#ModuleFiles` for trigger derivation; a Buildkite
two-commit base-branch build, a GitHub push with an uncomparable `before` and a merge-queue
run; a submodule under a reference; a no-break-space-padded path; a project whose ancestor
chain crosses a directory of another package.

Added in round 16: `e11` / `rootrel/` (a correct project whose patterns also match same-named
root files: no warning, no failure, the root match stops in B2); a worktree and a `.git` file under a reference;
`**` at a root project (no `.git` inputs).

Added in round 17: the three Buildkite cases of red-team r16 (promotion branch, tag build, a
branch that merged the default branch in); `CI=true` with no provider detected.

## 7. Docs, limitations, release note (PR-B1 for file references, PR-B2 for the rest)

New ADR superseding ADR-0007; run-tasks.mdx (~353); cue-schema.mdx (~694 stale
`#ExternalInput`/`#TaskRef`, ~2320 `../flake.nix`); how-to/ci.mdx; tutorials/monorepo.md;
reference/schema/status.md; skills `cuenv-tasks-graph-cache` (31-32) and `cuenv-ci-release`
(35); schema-coverage-matrix rows per PR as in §1's table.
Document: the three places a path can start from (own project: a string; another project:
`<pkg>.#Files & {paths: [...]}`; anywhere else in the module: `schema.#ModuleFiles & {paths:
[...]}` with paths from the module root); the caller chooses the files and the other project
declares nothing; every path must match at least one file; a `#Base` has no `#Files` — files
beside it are module files; `#Files` is not in scope unqualified in a project's own file;
spreading or iterating a reference's `paths` copies bare strings that re-anchor to the consumer —
never do it; a reference can be stored and re-exported (`let`, a field of a shared package) and
keeps its anchor; `["src/**", ...content.#Files & {paths: [...]}]` is valid CUE and silently
drops the reference — never spread one (cue-purist r12 N6); a reference cannot be optional;
`#Files` is a reserved definition on every project, and `#ProjectFiles` is the shape under it,
not something to write by hand; a file reference never follows a symlink or enters a submodule: under a glob they are left
out, and a path that goes through or ends at one is an error — name the target instead; nothing above the module root can be
named once `..` is rejected (red-team r12 N4); a `#Base`, or any file of an ancestor package,
cannot import a project below it (import cycle) — what lives there uses `schema.#ModuleFiles`;
at language v0.18.0 a base is written `schema.#Base...`, since an embedded `schema.#Base`
closes the package and rejects every child's `name`; below v0.18.0 that form does not parse
and plain `schema.#Base` works as before; a base with no schema embed works at every language;
each docs page and example shows one form, labelled with its language, and the release note
says which form matches the language pinned in `module.cue` (api-designer r14 N3); import form `content "<module>/content:cuenv"` (alias + `:cuenv`); a parent-directory
project cannot be imported; mutual project dependencies (package import cycle) need a third
**project**; two projects that read each other's files cannot both import the other — one side
uses `schema.#ModuleFiles` with the module-relative path; same CUE module only; a project's tasks are not templates (re-labelling a foreign
task, or putting one in a local sequence, conflicts on `_cuenvProject` with a multi-error
disjunction message; put reusable bodies in a definition in a non-project package); listing a
producer in both `dependsOn` and `inputs` is redundant; plain `cue export` shows `name: ""` for
sequence items (the bridge fills them); reserved names (`cuenvRef` as a group child,
`outputFiles` on tasks); deep fan-out `dependsOn` chains are slow under stock `cue export`;
groups cannot be inputs yet (list the children's `outputFiles`); new configs should use
`tasks.x.outputFiles` rather than `{task: "x"}`.
If the source check is deferred, the PR-B2 release note says to upgrade the schema before using
imports in `dependsOn` (on old schemas an import binds a same-named local task). The A
release note calls out that `{task}` inputs without `map` now stage at the producer's path.
Release note (breaking): `..` inputs rejected — import the project that holds the files and use
`<pkg>.#Files & {paths: [...]}`; files outside every project use `schema.#ModuleFiles & {paths:
[...]}` with paths from the module root;
`#ProjectReference` removed (PR-C); re-labelling a foreign task; `[..., ...content.tasks.x]` is
valid CUE but silently drops the reference; schema upgrade needed (`cue mod get`).
Validation: full `nix flake check`, `cuenv task ci.schema-docs-check`,
`cuenv task ci.schema-compat`, `cuenv sync ci --check`, CLI smoke tests, `cue vet ./schema`;
re-run all schema fixtures on CUE v0.18, specifically the guarded comprehension, the `let`
group stamp, the `error()` uses, the file-reference path checks and the `#Files` embedding form
(evaluator-sensitive). Service fixtures use
`entrypoint: schema.#Command & {...}`. Record timings on a quiet machine.

## 8. Follow-ups (filed)

Pre-existing exponential evaluation of chains of sequences and groups (PR-0's acceptance covers
task chains only; stamping adds 20–40% on top, not new growth — cue-purist r9).

Projects in a package other than the evaluated one as import targets (needs the
(directory, package) loader of rounds 6–10 and the cuengine same-directory import fix, old PR-D).

Filed from round 12 (pre-existing; not needed for this plan's guarantees): the guard for tasks
that republish their inputs (was PR-G); `cuenv sync ci` with module root ≠ checkout root (the
workflow and its paths are written under the module); a root `schema.#Base` embedding at
language v0.18.0 breaking every project below it (docs and examples must show the open form);
the bridge re-evaluating an imported project per importer; symlinks and git submodules under plain
string inputs (hashed through the link or beneath the gitlink, never affected in CI); `cuenv ci` ignoring `--package`;
one directory that fails to evaluate aborting every `cuenv ci` run; files above the module
root having no spelling once `..` is rejected; a warning for a plain string glob that matches
nothing; `--from` being ignored under a detected CI provider; a child directory that inherits
a project's name being planned and run as a second project; a base that only constrains
`name` counting as a project (`cuenv info`: Bases 0, Projects 2); a warning when a reference's walk leaves
out a link or nested repository that `.gitignore` does not cover (api-designer r15 N1); a
bind-time check for a string pattern that selects nothing under its project and something
from the checkout root, so the B2 failure does not wait for a change to touch the root file
(api-designer r15 N2).

Deferred from this plan: optional file references; CUE-text renderer hints for file
references (the mistakes and their real error lines are in round-12-api-designer.md, N1);
the source check of §3 (B2 shipped without it: an old pinned schema without stamps
can still bind an imported task to a same-named local one); R0's "unless p already
imports the consumer" refinement (B2's R0 message names the holding project only when
the evaluation knows it, and always gives the `schema.#ModuleFiles` form).

Named outputs for built files (spelling chosen then, not `artifact(s)`; file references already
cover files nobody builds); `outputFiles` on groups; cross-project
output refs, services, images, pipeline entries; job-per-task cross-project producers (run a
foreign producer inside the consumer's job: ci-engineer r6 (a)); deprecating
`{task: "..."}`; ancestor hooks under `--path`; absolute string inputs; plain `cue export`
slowness on deep `dependsOn` chains (stock CUE).
