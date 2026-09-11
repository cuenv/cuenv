// Run from any directory: node apps/cuetty/m0/validate-report.mjs
// This validates evidence bookkeeping, not terminal capabilities.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve, relative, isAbsolute } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const report = JSON.parse(readFileSync(new URL('./capabilities.json', import.meta.url), 'utf8'));
const statuses = new Set([
  'implemented_unvalidated', 'implemented_partially_validated', 'verified',
  'unsupported', 'deferred', 'blocked_by_rio_core', 'blocked_by_environment',
  'blocked_by_observed_failure',
]);
const outcomes = new Set(['pass', 'fail', 'unobservable']);
const required = [
  'public_api_build', 'pty_output_resize', 'final_output_child_status',
  'bounded_teardown', 'graphemes_wide_cells', 'styles_cursor_alt_screen',
  'native_history_selection_search', 'legacy_application_keys',
  'application_keypad_identity', 'bracketed_paste', 'mouse_wheel_reporting',
  'focus_reporting', 'hyperlink_destinations', 'title_bell_osc7', 'osc52_policy',
  'geometry_queries', 'color_queries', 'ime_dead_keys_option',
  'librio_control_regression', 'apple_silicon_workload', 'performance_history_budget',
];

function readEvidence(path) {
  assert.equal(typeof path, 'string');
  const absolute = resolve(root, path);
  const rel = relative(root, absolute);
  assert(rel && !rel.startsWith('..') && !isAbsolute(rel), `Unsafe evidence path: ${path}`);
  return readFileSync(absolute, 'utf8');
}

assert.equal(report.format_version, 1);
assert.equal(report.milestone, 'M0');
assert.equal(report.engine.package, 'rio-vt');
assert.equal(report.engine.revision, 'b0694c0707a90dc93fdf01cbf8a658424be285ee');
assert.equal(report.engine.rust_toolchain, '1.98.1');
assert.equal(report.control.revision, 'b0b79c1ebadc8d6a9a79c4c44a91a42b3ea439d1');
assert(Array.isArray(report.executions));
assert(Array.isArray(report.capabilities));
const executions = new Map();
for (const execution of report.executions) {
  assert(execution.id && !executions.has(execution.id), 'Execution IDs must be unique');
  assert(execution.command && execution.platform && execution.rust_toolchain && execution.executed_at);
  assert.match(execution.engine_revision, /^[0-9a-f]{40}$/);
  assert(Number.isInteger(execution.exit_code));
  assert(readEvidence(execution.log_path).trim(), 'Execution evidence needs a nonempty captured log');
  executions.set(execution.id, execution);
}
const capabilities = new Map();
for (const capability of report.capabilities) {
  assert(capability.id && !capabilities.has(capability.id), 'Capability IDs must be unique');
  assert.equal(typeof capability.required_for_m0, 'boolean');
  assert(statuses.has(capability.status), `Invalid status: ${capability.id}`);
  assert(outcomes.has(capability.outcome), `Invalid outcome: ${capability.id}`);
  assert(capability.notes && capability.gap_owner);
  assert(Array.isArray(capability.source_evidence));
  assert(Array.isArray(capability.execution_ids));
  for (const source of capability.source_evidence) {
    const text = readEvidence(source.path);
    if (source.symbol) assert(text.includes(source.symbol), `Missing symbol: ${source.symbol}`);
  }
  for (const source of capability.upstream_evidence ?? []) {
    assert(source.url.startsWith(`${report.engine.repository}/blob/${report.engine.revision}/`), 'Upstream evidence must pin the qualified revision');
    assert(Array.isArray(source.symbols) && source.symbols.length > 0);
  }
  if (capability.status === 'blocked_by_rio_core') {
    assert(capability.upstream_evidence?.length > 0, 'Source-inspected Rio blocker needs pinned upstream evidence');
    assert.notEqual(capability.outcome, 'pass');
  }
  const runs = capability.execution_ids.map(id => {
    assert(executions.has(id), `Missing execution: ${id}`);
    return executions.get(id);
  });
  const currentRuns = runs.filter(run => run.engine_revision === report.engine.revision);
  if (capability.status === 'implemented_unvalidated') {
    assert(capability.source_evidence.length > 0, `Implementation needs source evidence: ${capability.id}`);
  }
  if (capability.status === 'verified' || capability.outcome === 'pass') {
    assert.equal(capability.status, 'verified');
    assert.equal(capability.outcome, 'pass');
    assert(currentRuns.some(run => run.exit_code === 0), `Pass needs a successful current-revision execution: ${capability.id}`);
  } else if (capability.outcome === 'fail') {
    assert(currentRuns.some(run => run.exit_code !== 0), `Failure needs a failed current-revision execution: ${capability.id}`);
  }
  if (capability.status === 'blocked_by_observed_failure') {
    assert.equal(capability.outcome, 'fail');
    assert(currentRuns.some(run => run.result === 'failed'), `Observed failure needs a failed current-revision test, not just a blocked build: ${capability.id}`);
  }
  capabilities.set(capability.id, capability);
}
for (const id of required) assert.equal(capabilities.get(id)?.required_for_m0, true, `Missing M0 requirement: ${id}`);
for (const id of ['advanced_keyboard', 'kitty_graphics']) assert(capabilities.has(id), `Missing explicit scope: ${id}`);
const passed = report.capabilities.filter(capability => capability.required_for_m0).every(capability => capability.outcome === 'pass');
assert.equal(report.qualification, passed ? 'passed' : 'not_passed');
console.log(`Valid M0 evidence report: ${capabilities.size} capabilities, ${executions.size} recorded executions; qualification ${report.qualification}.`);
