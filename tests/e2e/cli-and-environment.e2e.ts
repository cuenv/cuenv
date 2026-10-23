import { expect } from "e2e";

import { test } from "./support/fixtures.ts";

const POSIX_PLATFORMS = ["darwin", "linux"] as const;
const EXIT_CLI = 2;
const EXIT_EVAL = 3;

const project = (body: string) => `package test

import "github.com/cuenv/cuenv/schema"

schema.#Project

${body}
`;

function parseEnvOutput(output: string): Record<string, string> {
  return Object.fromEntries(
    output
      .trim()
      .split(/\r?\n/)
      .filter(Boolean)
      .map((line) => {
        const separator = line.indexOf("=");
        if (separator < 1) throw new Error(`invalid env output line: ${line}`);
        return [line.slice(0, separator), line.slice(separator + 1)];
      }),
  );
}

test(
  "CLI-001 help exposes the primary command surface",
  { platforms: POSIX_PLATFORMS, tags: ["smoke", "cli"] },
  async ({ workspace }) => {
    const result = await workspace.run(["--help"]);

    expect(result.exitCode).toBe(0);
    expect(result.stdout).toContain("Usage:");
    expect(result.stdout).toContain("task");
    expect(result.stdout).toContain("env");
  },
);

test(
  "ENV-001 env print preserves exact values in env and JSON formats",
  { platforms: POSIX_PLATFORMS, tags: ["smoke", "environment"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "environment"

env: {
	API_URL: "https://example.test/api"
	EMPTY:   ""
	UNICODE: "café 🌱"
	EQUALS:  "left=right"
}`),
    );

    const text = await workspace.run([
      "env",
      "print",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "--output",
      "env",
    ]);
    const json = await workspace.run([
      "env",
      "print",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "--output",
      "json",
    ]);

    expect(text.exitCode).toBe(0);
    expect(parseEnvOutput(text.stdout)).toEqual({
      API_URL: "https://example.test/api",
      EMPTY: "",
      EQUALS: "left=right",
      UNICODE: "café 🌱",
    });
    expect(json.exitCode).toBe(0);
    expect(JSON.parse(json.stdout)).toEqual({
      API_URL: "https://example.test/api",
      EMPTY: "",
      EQUALS: "left=right",
      UNICODE: "café 🌱",
    });
  },
);

test(
  "ENV-002 named environments override the base environment",
  { platforms: POSIX_PLATFORMS, tags: ["acceptance", "environment"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "environment-overrides"

env: {
	API_URL: "https://example.test/api"
	MODE:    "base"
	BASE_ONLY: "preserved"
	environment: {
		dev: {
			API_URL: "https://dev.example.test/api"
			MODE:    "development"
		}
	}
}`),
    );

    const result = await workspace.run([
      "env",
      "print",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "--env",
      "dev",
      "--output",
      "json",
    ]);

    expect(result.exitCode).toBe(0);
    expect(JSON.parse(result.stdout)).toEqual({
      API_URL: "https://dev.example.test/api",
      MODE: "development",
      BASE_ONLY: "preserved",
    });
  },
);

test(
  "CLI-002 invalid CUE is an evaluation error",
  { platforms: POSIX_PLATFORMS, tags: ["acceptance", "negative"] },
  async ({ workspace }) => {
    await workspace.writeEnv(`package test

env: {
	BROKEN: "unterminated
`);

    const result = await workspace.run([
      "env",
      "print",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "--output",
      "json",
    ]);

    expect(result.exitCode).toBe(EXIT_EVAL);
    expect(result.signal).toBe(null);
    expect(result.timedOut).toBe(false);
    expect(result.combined).toContain("Evaluation/FFI error");
    expect(result.combined).toContain("string literal not terminated");
  },
);

test(
  "CLI-003 unknown tasks name the requested task",
  { platforms: POSIX_PLATFORMS, tags: ["acceptance", "negative"] },
  async ({ workspace }) => {
    await workspace.writeEnv(
      project(`name: "unknown-task"

tasks: {
	ok: schema.#Task & {
		command: "echo"
		args: ["ok"]
	}
}`),
    );

    const result = await workspace.run([
      "task",
      "--path",
      workspace.projectDir,
      "--package",
      "test",
      "missing",
    ]);

    expect(result.exitCode).toBe(EXIT_CLI);
    expect(result.signal).toBe(null);
    expect(result.timedOut).toBe(false);
    expect(result.combined).toContain("Task 'missing' not found.");
  },
);
