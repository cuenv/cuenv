import { expect } from "e2e";

import { test } from "./support/fixtures.ts";

const POSIX_PLATFORMS = ["darwin", "linux"] as const;
const EXIT_EVAL = 3;

const project = (body: string) => `package test

import "github.com/cuenv/cuenv/schema"

schema.#Project

${body}
`;

test(
  "TASK-001 dependencies execute in declared order",
  { platforms: POSIX_PLATFORMS, tags: ["smoke", "tasks"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "task-order"

let _tasks = tasks

tasks: {
	first: schema.#Task & {
		command:  "sh"
		args:     ["-c", "echo first >> order.log"]
		hermetic: false
	}
	second: schema.#Task & {
		command:  "sh"
		args:     ["-c", "echo second >> order.log"]
		hermetic: false
		dependsOn: [_tasks.first]
	}
}`),
    );

    const result = await workspace.run([
      "task",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "second",
    ]);

    expect(result.exitCode).toBe(0);
    expect(await workspace.read("order.log")).toBe("first\nsecond\n");
  },
);

test(
  "TASK-002 failed dependencies prevent dependent side effects",
  { platforms: POSIX_PLATFORMS, tags: ["acceptance", "tasks", "negative"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "task-failure"

let _tasks = tasks

tasks: {
	fail: schema.#Task & {
		command:  "sh"
		args:     ["-c", "echo failed > failed.log; exit 7"]
		hermetic: false
	}
	dependent: schema.#Task & {
		command:  "sh"
		args:     ["-c", "echo dependent > dependent.log"]
		hermetic: false
		dependsOn: [_tasks.fail]
	}
}`),
    );

    const result = await workspace.run([
      "task",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "dependent",
    ]);

    expect(result.exitCode).toBe(EXIT_EVAL);
    expect(result.signal).toBe(null);
    expect(result.timedOut).toBe(false);
    expect(await workspace.read("failed.log")).toBe("failed\n");
    expect(await workspace.exists("dependent.log")).toBe(false);
  },
);

test(
  "TASK-003 task groups execute every child",
  { platforms: POSIX_PLATFORMS, tags: ["acceptance", "tasks"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "task-group"

tasks: {
	all: {
		type: "group"
		one: schema.#Task & {
			command:  "sh"
			args:     ["-c", "echo one > one.log"]
			hermetic: false
		}
		two: schema.#Task & {
			command:  "sh"
			args:     ["-c", "echo two > two.log"]
			hermetic: false
		}
	}
}`),
    );

    const result = await workspace.run([
      "task",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "all",
    ]);

    expect(result.exitCode).toBe(0);
    expect(await workspace.read("one.log")).toBe("one\n");
    expect(await workspace.read("two.log")).toBe("two\n");
  },
);

test(
  "TASK-004 JSON task listing exposes the resolved task set",
  { platforms: POSIX_PLATFORMS, tags: ["acceptance", "tasks"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "task-list"

tasks: {
	alpha: schema.#Task & {
		command: "echo"
		args:    ["alpha"]
	}
	beta: schema.#Task & {
		command: "echo"
		args:    ["beta"]
	}
}`),
    );

    const result = await workspace.run([
      "task",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "--output",
      "json",
    ]);

    expect(result.exitCode).toBe(0);
    const tasks = JSON.parse(result.stdout) as Array<{ name: string }>;
    expect(tasks.map(({ name }) => name).sort()).toEqual(["alpha", "beta"]);
  },
);

test(
  "TASK-005 timeout cleanup terminates a task process group",
  { platforms: POSIX_PLATFORMS, tags: ["acceptance", "processes", "negative"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "task-timeout"

tasks: {
	slow: schema.#Task & {
		command:  "sh"
		args:     ["-c", "trap '' TERM INT; sleep 30 & child=$!; echo $$ > leader.pid; echo $child > child.pid; echo ready > ready; wait $child"]
		hermetic: false
	}
}`),
    );

    const result = await workspace.run(
      [
        "task",
        "--path",
        workspace.projectDir,
        "--package",
        "test",
        "slow",
      ],
      {
        readinessFile: "ready",
        readinessTimeoutMs: 5_000,
        timeoutMs: 1_000,
        trackProcessGroups: ["leader.pid"],
        trackProcesses: ["child.pid"],
      },
    );

    const leaderPid = Number.parseInt((await workspace.read("leader.pid")).trim(), 10);
    const childPid = Number.parseInt((await workspace.read("child.pid")).trim(), 10);
    expect(Number.isInteger(leaderPid)).toBe(true);
    expect(Number.isInteger(childPid)).toBe(true);
    workspace.trackProcessGroup(leaderPid);
    workspace.trackProcess(childPid);

    expect(result.exitCode).toBe(130);
    expect(result.signal).toBe(null);
    expect(result.timedOut).toBe(true);
    expect(await workspace.waitForProcessExit(childPid)).toBe(true);
    expect(await workspace.waitForProcessGroupExit(leaderPid)).toBe(true);
  },
);
