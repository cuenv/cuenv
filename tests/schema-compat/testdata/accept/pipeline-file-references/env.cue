package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

// A pipeline entry and a `dependsOn` entry are unified with #TaskNode. A
// task whose inputs hold a file reference must cost there what a task with
// plain path inputs costs. A schema that spelled the members of a group's
// sequence out as a fresh disjunction made this export take minutes under
// CUE v0.17; the compat script stops an evaluation that long and fails.
//
// `_content` stands for an imported project: its #Files is the same value
// whether the project is reached through an import or not.
let _content = schema.#Project & {name: "content"}
let _tasks = tasks

ci: pipelines: default: tasks: [_tasks.project, _tasks.module]

tasks: {
	project: {command: "true", inputs: ["src/**", _content.#Files & {paths: ["articles/**"]}]}
	module: {command: "true", inputs: ["src/**", schema.#ModuleFiles & {paths: ["bun.lock"]}]}
	publish: {command: "true", dependsOn: [_tasks.project, _tasks.module]}
}
