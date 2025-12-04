"use strict";
var __create = Object.create;
var __defProp = Object.defineProperty;
var __getOwnPropDesc = Object.getOwnPropertyDescriptor;
var __getOwnPropNames = Object.getOwnPropertyNames;
var __getProtoOf = Object.getPrototypeOf;
var __hasOwnProp = Object.prototype.hasOwnProperty;
var __export = (target, all) => {
  for (var name in all)
    __defProp(target, name, { get: all[name], enumerable: true });
};
var __copyProps = (to, from, except, desc) => {
  if (from && typeof from === "object" || typeof from === "function") {
    for (let key of __getOwnPropNames(from))
      if (!__hasOwnProp.call(to, key) && key !== except)
        __defProp(to, key, { get: () => from[key], enumerable: !(desc = __getOwnPropDesc(from, key)) || desc.enumerable });
  }
  return to;
};
var __toESM = (mod, isNodeMode, target) => (target = mod != null ? __create(__getProtoOf(mod)) : {}, __copyProps(
  // If the importer is in node compatibility mode or this is not an ESM
  // file that has been converted to a CommonJS file using a Babel-
  // compatible transform (i.e. "__esModule" has not been set), then set
  // "default" to the CommonJS "module.exports" for node compatibility.
  isNodeMode || !mod || !mod.__esModule ? __defProp(target, "default", { value: mod, enumerable: true }) : target,
  mod
));
var __toCommonJS = (mod) => __copyProps(__defProp({}, "__esModule", { value: true }), mod);

// src/extension.ts
var extension_exports = {};
__export(extension_exports, {
  activate: () => activate,
  deactivate: () => deactivate
});
module.exports = __toCommonJS(extension_exports);
var vscode = __toESM(require("vscode"));
var cp = __toESM(require("child_process"));
function activate(context) {
  const outputChannel = vscode.window.createOutputChannel("Cuenv");
  const client = new CuenvClient(outputChannel);
  const taskProvider = new TaskTreeDataProvider(client);
  const envProvider = new EnvTreeDataProvider(client);
  vscode.window.registerTreeDataProvider("cuenv.tasks", taskProvider);
  vscode.window.registerTreeDataProvider("cuenv.environments", envProvider);
  context.subscriptions.push(
    vscode.commands.registerCommand("cuenv.refresh", () => {
      taskProvider.refresh();
      envProvider.refresh();
    }),
    vscode.commands.registerCommand("cuenv.runTask", (item) => {
      client.runTask(item.label);
    }),
    vscode.commands.registerCommand("cuenv.setEnvironment", (item) => {
      client.setEnvironment(item.label);
      envProvider.refresh();
    })
  );
  taskProvider.refresh();
  envProvider.refresh();
}
function deactivate() {
}
var CuenvClient = class {
  constructor(outputChannel) {
    this.outputChannel = outputChannel;
  }
  getExecutable() {
    return vscode.workspace.getConfiguration("cuenv").get("executablePath") || "cuenv";
  }
  getWorkspaceRoot() {
    return vscode.workspace.workspaceFolders?.[0].uri.fsPath;
  }
  setEnvironment(env) {
    this.currentEnv = env;
    vscode.window.showInformationMessage(`Cuenv environment set to: ${env}`);
  }
  async getTasks() {
    const root = this.getWorkspaceRoot();
    if (!root)
      return [];
    try {
      const output = await this.execJson(["task", "--output-format", "json"], root);
      return output;
    } catch (e) {
      this.outputChannel.appendLine(`Error fetching tasks: ${e}`);
      return [];
    }
  }
  async getEnvironments() {
    const root = this.getWorkspaceRoot();
    if (!root)
      return [];
    try {
      const output = await this.execJson(["env", "list", "--output-format", "json"], root);
      return output;
    } catch (e) {
      this.outputChannel.appendLine(`Error fetching environments: ${e}`);
      return [];
    }
  }
  runTask(taskName) {
    const root = this.getWorkspaceRoot();
    if (!root)
      return;
    const executable = this.getExecutable();
    const args = ["task", taskName];
    if (this.currentEnv) {
      args.push("--env", this.currentEnv);
    }
    const terminal = vscode.window.createTerminal({
      name: `Cuenv: ${taskName}`,
      cwd: root,
      env: process.env
      // Inherit env to ensure path is correct
    });
    terminal.show();
    terminal.sendText(`${executable} ${args.join(" ")}`);
  }
  execJson(args, cwd) {
    return new Promise((resolve, reject) => {
      const executable = this.getExecutable();
      cp.execFile(executable, args, { cwd }, (error, stdout, stderr) => {
        if (error) {
          reject(stderr || error.message);
          return;
        }
        try {
          resolve(JSON.parse(stdout));
        } catch (e) {
          reject(`Failed to parse JSON: ${e}
Output: ${stdout}`);
        }
      });
    });
  }
  getCurrentEnvironment() {
    return this.currentEnv;
  }
};
var TaskTreeDataProvider = class {
  constructor(client) {
    this.client = client;
    this._onDidChangeTreeData = new vscode.EventEmitter();
    this.onDidChangeTreeData = this._onDidChangeTreeData.event;
  }
  refresh() {
    this._onDidChangeTreeData.fire();
  }
  getTreeItem(element) {
    return element;
  }
  async getChildren(element) {
    if (element) {
      return [];
    }
    const tasks = await this.client.getTasks();
    return tasks.map((t) => new TaskTreeItem(
      t.name,
      t.description || (t.is_group ? "Task Group" : "Task"),
      t.is_group ? vscode.TreeItemCollapsibleState.None : vscode.TreeItemCollapsibleState.None
    ));
  }
};
var TaskTreeItem = class extends vscode.TreeItem {
  constructor(label, description, collapsibleState) {
    super(label, collapsibleState);
    this.label = label;
    this.description = description;
    this.collapsibleState = collapsibleState;
    this.tooltip = `${this.label}: ${this.description}`;
    this.contextValue = "task";
    this.iconPath = new vscode.ThemeIcon("checklist");
  }
};
var EnvTreeDataProvider = class {
  constructor(client) {
    this.client = client;
    this._onDidChangeTreeData = new vscode.EventEmitter();
    this.onDidChangeTreeData = this._onDidChangeTreeData.event;
  }
  refresh() {
    this._onDidChangeTreeData.fire();
  }
  getTreeItem(element) {
    return element;
  }
  async getChildren(element) {
    if (element)
      return [];
    const envs = await this.client.getEnvironments();
    const current = this.client.getCurrentEnvironment();
    return envs.map((e) => new EnvTreeItem(e, e === current));
  }
};
var EnvTreeItem = class extends vscode.TreeItem {
  constructor(label, isActive) {
    super(label, vscode.TreeItemCollapsibleState.None);
    this.label = label;
    this.isActive = isActive;
    this.contextValue = "environment";
    if (isActive) {
      this.iconPath = new vscode.ThemeIcon("check");
      this.description = "(Active)";
    } else {
      this.iconPath = new vscode.ThemeIcon("server");
    }
  }
};
// Annotate the CommonJS export names for ESM import in node:
0 && (module.exports = {
  activate,
  deactivate
});
