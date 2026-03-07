# GitHub Issues Audit (cuenv/cuenv)

Date: 2026-03-07
Auditor: Product Owner (Codex)

## Scope

- Audited open issues in `cuenv/cuenv` via GitHub REST API.
- Cross-checked high-signal issues against current code to determine whether they are duplicates, still valid, or partially implemented.

## Blocking Limitation

The environment does not have GitHub write credentials, so issue state/comment updates cannot be applied directly from this session.

- Attempted issue state change API call result: `Requires authentication`.

## Duplicate Issues To Close

These pairs are functionally duplicates (same title and same code references), with the higher-number issue containing richer implementation context.

1. Close **#288** as duplicate of **#293** (GitLab CI emitter).
2. Close **#289** as duplicate of **#294** (CircleCI emitter).
3. Close **#290** as duplicate of **#295** (CI matrix filter).
4. Close **#291** as duplicate of **#296** (CI jobs limit).
5. Close **#292** as duplicate of **#297** (release tag config from CUE).

## Current Implementation Findings

### Still Open / Not Implemented (confirmed by code)

- **GitLab CI export emitter** remains TODO and returns a not-implemented configuration error.
  - Relevant issue: #293 (and duplicate #288).
- **CircleCI export emitter** remains TODO and returns a not-implemented configuration error.
  - Relevant issue: #294 (and duplicate #289).
- **CI runner matrix filter** still has TODO and logs “not yet fully implemented”.
  - Relevant issue: #295 (and duplicate #290).
- **CI runner jobs limit** still has TODO placeholder and no actual limiting implementation.
  - Relevant issue: #296 (and duplicate #291).
- **Release tag config from CUE** still contains TODOs in release command and hardcoded fallback behavior.
  - Relevant issue: #297 (and duplicate #292).

### Likely Already Implemented (issue may need status refresh)

- **Affected-task detection groundwork exists** in core and task graph:
  - `AffectedBy` trait and matching logic are implemented.
  - Task graph includes `compute_affected` plus tests.
  - Related issue likely needing refresh: #148 (task filtering/affected detection).

## Recommended Issue Updates

Use the following updates in GitHub:

- For #293, #294, #295, #296, #297:
  - Add comment: “Re-audited against current `main`; TODO stubs still present in CLI implementation. Keeping open.”
  - Add labels: `status:confirmed`, `area:ci` (and `area:release` for #297).
- For #148:
  - Add comment: “Core affected detection is implemented; remaining work should be narrowed to CLI UX/pattern filtering scope if still missing.”
  - Consider splitting residual work into smaller follow-ups and close #148 if scope is now substantially complete.

## Suggested One-Time Bulk Maintenance

1. Close duplicates: #288, #289, #290, #291, #292.
2. Keep canonical issues open: #293, #294, #295, #296, #297.
3. Re-scope #148 to explicitly define remaining gaps vs implemented core behavior.
4. Add a project board field `audit:last-reviewed` and set it for all open issues touched in this pass.

