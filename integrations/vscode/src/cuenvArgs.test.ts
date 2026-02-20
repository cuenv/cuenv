import { strict as assert } from 'node:assert';
import { test } from 'node:test';
import {
    BASE_ENVIRONMENT,
    buildEnvironmentListArgs,
    buildEnvironmentPrintArgs,
    buildTaskListArgs,
    buildTaskRunArgs,
    buildWorkspaceTaskListArgs
} from './cuenvArgs';

test('buildTaskListArgs uses --output json', () => {
    assert.deepEqual(buildTaskListArgs(), ['task', '--output', 'json']);
});

test('buildTaskListArgs prefixes non-Base env as global flag', () => {
    assert.deepEqual(buildTaskListArgs('development'), [
        '--env',
        'development',
        'task',
        '--output',
        'json'
    ]);
});

test('buildWorkspaceTaskListArgs uses --all and --output json', () => {
    assert.deepEqual(buildWorkspaceTaskListArgs(), ['task', '--all', '--output', 'json']);
});

test('buildWorkspaceTaskListArgs prefixes non-Base env as global flag', () => {
    assert.deepEqual(buildWorkspaceTaskListArgs('development'), [
        '--env',
        'development',
        'task',
        '--all',
        '--output',
        'json'
    ]);
});

test('buildEnvironmentListArgs uses --output json', () => {
    assert.deepEqual(buildEnvironmentListArgs(), ['env', 'list', '--output', 'json']);
});

test('buildEnvironmentPrintArgs keeps Base environment unscoped', () => {
    assert.deepEqual(buildEnvironmentPrintArgs(BASE_ENVIRONMENT), ['env', 'print', '--output', 'json']);
});

test('buildEnvironmentPrintArgs prefixes non-Base env as global flag', () => {
    assert.deepEqual(buildEnvironmentPrintArgs('production'), [
        '--env',
        'production',
        'env',
        'print',
        '--output',
        'json'
    ]);
});

test('buildTaskRunArgs prefixes non-Base env before task command', () => {
    assert.deepEqual(buildTaskRunArgs('deploy', 'staging'), [
        '--env',
        'staging',
        'task',
        'deploy'
    ]);
});

test('buildTaskRunArgs keeps Base environment unscoped', () => {
    assert.deepEqual(buildTaskRunArgs('deploy', BASE_ENVIRONMENT), ['task', 'deploy']);
});
