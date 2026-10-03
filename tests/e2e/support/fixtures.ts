import { spawn, type ChildProcess } from "node:child_process";
import { constants } from "node:fs";
import { fileURLToPath } from "node:url";
import {
  access,
  cp,
  mkdtemp,
  mkdir,
  readFile,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join, relative, resolve, sep } from "node:path";
import { StringDecoder } from "node:string_decoder";

import { test as base } from "e2e";

const CAPTURE_LIMIT = 256 * 1024;
// cuenv's Ctrl-C handler allows five seconds for its registered task groups;
// keep the wrapper grace longer so the fallback cannot race that cleanup.
const INTERRUPT_GRACE_MS = 7_000;
const FORCE_GRACE_MS = 1_000;
const TRACKED_PROCESS_GRACE_MS = 2_000;

export interface CommandResult {
  readonly args: readonly string[];
  readonly combined: string;
  readonly exitCode: number | null;
  readonly signal: NodeJS.Signals | null;
  readonly stderr: string;
  readonly stdout: string;
  readonly timedOut: boolean;
}

export interface RunOptions {
  readonly readinessFile?: string;
  readonly readinessTimeoutMs?: number;
  readonly timeoutMs?: number;
  readonly trackProcessGroups?: readonly string[];
  readonly trackProcesses?: readonly string[];
}

interface ProcessHandle {
  readonly child: ChildProcess;
  readonly completion: Promise<CommandResult>;
  readonly registerTrackedProcesses: () => Promise<void>;
  readonly terminate: () => Promise<void>;
}

export interface CuenvWorkspace {
  readonly binary: string;
  readonly projectDir: string;
  readonly rootDir: string;
  read(relativePath: string): Promise<string>;
  run(args: readonly string[], options?: RunOptions): Promise<CommandResult>;
  writeEnv(content: string): Promise<void>;
  exists(relativePath: string): Promise<boolean>;
  trackProcess(pid: number): void;
  trackProcessGroup(pid: number): void;
  waitForProcessExit(pid: number, timeoutMs?: number): Promise<boolean>;
  waitForProcessGroupExit(pid: number, timeoutMs?: number): Promise<boolean>;
  cleanup(): Promise<void>;
}

function appendBounded(current: string, chunk: Buffer | string): string {
  if (current.length >= CAPTURE_LIMIT) return current;
  const next = current + chunk.toString();
  return next.length <= CAPTURE_LIMIT
    ? next
    : `${next.slice(0, CAPTURE_LIMIT)}\n[output truncated by cuenv e2e fixture]`;
}

function waitFor(milliseconds: number): Promise<void> {
  return new Promise((resolvePromise) => {
    setTimeout(resolvePromise, milliseconds);
  });
}

async function completedWithin(
  completion: Promise<CommandResult>,
  milliseconds: number,
): Promise<boolean> {
  return Promise.race([
    completion.then(() => true),
    waitFor(milliseconds).then(() => false),
  ]);
}

function sendSignal(
  child: ChildProcess,
  signal: NodeJS.Signals,
): void {
  if (child.pid === undefined) return;

  try {
    child.kill(signal);
  } catch (error) {
    if (!(error instanceof Error) || !("code" in error) || error.code !== "ESRCH") {
      throw error;
    }
  }
}

function sendProcessGroupSignal(pid: number, signal: NodeJS.Signals): void {
  try {
    if (process.platform !== "win32") {
      process.kill(-pid, signal);
    } else {
      process.kill(pid, signal);
    }
  } catch (error) {
    if (!(error instanceof Error) || !("code" in error) || error.code !== "ESRCH") {
      throw error;
    }
  }
}

function sendProcessSignal(pid: number, signal: NodeJS.Signals): void {
  try {
    process.kill(pid, signal);
  } catch (error) {
    if (!(error instanceof Error) || !("code" in error) || error.code !== "ESRCH") {
      throw error;
    }
  }
}

function isProcessAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ESRCH") {
      return false;
    }
    throw error;
  }
}

async function waitForProcessExit(
  pid: number,
  timeoutMs: number,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (isProcessAlive(pid) && Date.now() < deadline) {
    await waitFor(50);
  }
  return !isProcessAlive(pid);
}

function isProcessGroupAlive(pid: number): boolean {
  try {
    if (process.platform !== "win32") {
      process.kill(-pid, 0);
    } else {
      process.kill(pid, 0);
    }
    return true;
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ESRCH") {
      return false;
    }
    throw error;
  }
}

async function waitForProcessGroupExit(
  pid: number,
  timeoutMs: number,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (isProcessGroupAlive(pid) && Date.now() < deadline) {
    await waitFor(50);
  }
  return !isProcessGroupAlive(pid);
}

async function resolveCuenvBinary(): Promise<string> {
  const configured = process.env.CUENV_TEST_BIN;
  if (!configured) {
    throw new Error(
      "CUENV_TEST_BIN must be set to an absolute, executable cuenv binary; " +
        "build it once before running the e2e suite",
    );
  }
  if (!isAbsolute(configured)) {
    throw new Error(`CUENV_TEST_BIN must be absolute, got ${configured}`);
  }

  let details: Awaited<ReturnType<typeof stat>>;
  try {
    details = await stat(configured);
    await access(configured, constants.X_OK);
  } catch (error) {
    throw new Error(`CUENV_TEST_BIN is not an executable file: ${configured}`, {
      cause: error,
    });
  }
  if (!details.isFile()) {
    throw new Error(`CUENV_TEST_BIN is not a file: ${configured}`);
  }
  return configured;
}

function projectPath(projectDir: string, relativePath: string): string {
  if (isAbsolute(relativePath)) {
    throw new Error(`fixture paths must be relative: ${relativePath}`);
  }
  const target = resolve(projectDir, relativePath);
  const escapedPath = relative(projectDir, target);
  const escaped = escapedPath === ".." || escapedPath.startsWith(`..${sep}`);
  if (escaped || target === projectDir) {
    throw new Error(`fixture path escapes project directory: ${relativePath}`);
  }
  return target;
}

async function createWorkspace(): Promise<CuenvWorkspace> {
  const binary = await resolveCuenvBinary();
  const rootDir = await mkdtemp(join(tmpdir(), "cuenv-e2e-"));
  const projectDir = join(rootDir, "project");
  const homeDir = join(rootDir, "home");
  const stateDir = join(rootDir, "state");
  const cacheDir = join(rootDir, "cache");
  const runtimeDir = join(rootDir, "runtime");
  const tempDir = join(rootDir, "tmp");

  try {
    const setupResults = await Promise.allSettled([
      mkdir(projectDir, { recursive: true }),
      mkdir(homeDir, { recursive: true }),
      mkdir(stateDir, { recursive: true }),
      mkdir(cacheDir, { recursive: true }),
      mkdir(runtimeDir, { recursive: true }),
      mkdir(tempDir, { recursive: true }),
    ]);
    const setupErrors = setupResults
      .filter((result): result is PromiseRejectedResult => result.status === "rejected")
      .map(({ reason }) => reason);
    if (setupErrors.length > 0) {
      throw new AggregateError(setupErrors, "cuenv e2e fixture directory setup failed");
    }

    const repoRoot = fileURLToPath(new URL("../../../", import.meta.url));
    await cp(join(repoRoot, "schema"), join(projectDir, "schema"), {
      recursive: true,
    });
    await mkdir(join(projectDir, "cue.mod"), { recursive: true });
    await writeFile(
      join(projectDir, "cue.mod", "module.cue"),
      'module: "github.com/cuenv/cuenv"\nlanguage: version: "v0.14.1"\n',
    );
  } catch (error) {
    try {
      await rm(rootDir, { force: true, recursive: true });
    } catch (rollbackError) {
      throw new AggregateError(
        [error, rollbackError],
        "cuenv e2e fixture setup and rollback both failed",
      );
    }
    throw error;
  }

  const environment: NodeJS.ProcessEnv = {
    PATH: process.env.PATH,
    HOME: homeDir,
    USER: "cuenv-e2e",
    LOGNAME: "cuenv-e2e",
    TMPDIR: tempDir,
    TMP: tempDir,
    TEMP: tempDir,
    LANG: "C",
    LC_ALL: "C",
    TERM: "dumb",
    NO_COLOR: "1",
    XDG_CONFIG_HOME: join(homeDir, "config"),
    XDG_CACHE_HOME: join(homeDir, "cache"),
    XDG_DATA_HOME: join(homeDir, "data"),
    XDG_STATE_HOME: join(homeDir, "state"),
    CUENV_STATE_DIR: stateDir,
    CUENV_CACHE_DIR: cacheDir,
    CUENV_RUNTIME_DIR: runtimeDir,
    CUENV_APPROVAL_FILE: join(stateDir, "approved.json"),
    CUENV_EXECUTABLE: binary,
    CUENV_CACHE: "off",
    CUENV_TEST_BIN: binary,
  };

  let closed = false;
  let cleanupPromise: Promise<void> | undefined;
  const processes = new Set<ProcessHandle>();
  const trackedProcesses = new Set<number>();
  const trackedProcessGroups = new Set<number>();

  const run = async (
    args: readonly string[],
    options: RunOptions = {},
  ): Promise<CommandResult> => {
    if (closed) throw new Error("cuenv e2e fixture is already closed");

    const child = spawn(binary, [...args], {
      cwd: projectDir,
      detached: process.platform !== "win32",
      env: environment,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    const stdoutStream = child.stdout;
    const stderrStream = child.stderr;
    if (!stdoutStream || !stderrStream) {
      child.kill();
      throw new Error("cuenv e2e fixture could not capture child output");
    }

    let stdout = "";
    let stderr = "";
    let settled = false;
    let resolveCompletion!: (result: CommandResult) => void;
    let timedOut = false;
    const stdoutDecoder = new StringDecoder("utf8");
    const stderrDecoder = new StringDecoder("utf8");

    const completion = new Promise<CommandResult>((resolveCompletionPromise) => {
      resolveCompletion = resolveCompletionPromise;
    });

    const finish = (exitCode: number | null, signal: NodeJS.Signals | null) => {
      if (settled) return;
      settled = true;
      stdout = appendBounded(stdout, stdoutDecoder.end());
      stderr = appendBounded(stderr, stderrDecoder.end());
      resolveCompletion({
        args,
        combined: stdout + stderr,
        exitCode,
        signal,
        stderr,
        stdout,
        timedOut,
      });
    };

    stdoutStream.on("data", (chunk: Buffer | string) => {
      stdout = appendBounded(
        stdout,
        typeof chunk === "string" ? chunk : stdoutDecoder.write(chunk),
      );
    });
    stderrStream.on("data", (chunk: Buffer | string) => {
      stderr = appendBounded(
        stderr,
        typeof chunk === "string" ? chunk : stderrDecoder.write(chunk),
      );
    });
    child.once("error", (error) => {
      stderr = appendBounded(stderr, `${error.message}\n`);
      finish(null, null);
    });
    child.once("close", (exitCode, signal) => finish(exitCode, signal));

    let terminationPromise: Promise<void> | undefined;
    const terminate = (): Promise<void> => {
      if (terminationPromise) return terminationPromise;
      terminationPromise = (async () => {
        if (settled) return;

        // cuenv owns task process groups separately from this wrapper. Give
        // cuenv SIGINT first so its own cancellation registry can clean them.
        sendSignal(child, "SIGINT");
        if (await completedWithin(completion, INTERRUPT_GRACE_MS)) return;

        // The detached wrapper group is only a bounded fallback for a cuenv
        // process that did not complete its own cleanup.
        if (child.pid !== undefined) {
          sendProcessGroupSignal(child.pid, "SIGTERM");
        }
        if (await completedWithin(completion, FORCE_GRACE_MS)) return;
        if (child.pid !== undefined) {
          sendProcessGroupSignal(child.pid, "SIGKILL");
        }
        if (await completedWithin(completion, FORCE_GRACE_MS)) return;

        // A descendant can keep the stdio pipes open after the wrapper dies.
        // Resolve the operation as a cleanup failure instead of hanging the
        // test runner indefinitely; tracked descendants are handled below.
        stdoutStream.destroy();
        stderrStream.destroy();
        finish(null, "SIGKILL");
        throw new Error("cuenv process did not close after forced cleanup");
      })();
      return terminationPromise;
    };

    let registerConfiguredProcesses: () => Promise<void> = async () => undefined;
    const processHandle: ProcessHandle = {
      child,
      completion,
      registerTrackedProcesses: () => registerConfiguredProcesses(),
      terminate,
    };
    processes.add(processHandle);

    const readPid = async (relativePath: string): Promise<number | undefined> => {
      let content: string;
      try {
        content = await readFile(projectPath(projectDir, relativePath), "utf8");
      } catch (error) {
        if (error instanceof Error && "code" in error && error.code === "ENOENT") {
          return undefined;
        }
        throw error;
      }
      const pid = Number.parseInt(content.trim(), 10);
      if (!Number.isInteger(pid) || pid <= 0) {
        throw new Error(`fixture PID file did not contain a positive integer: ${relativePath}`);
      }
      return pid;
    };

    registerConfiguredProcesses = async (): Promise<void> => {
      for (const relativePath of options.trackProcessGroups ?? []) {
        const pid = await readPid(relativePath);
        if (pid !== undefined) trackedProcessGroups.add(pid);
      }
      for (const relativePath of options.trackProcesses ?? []) {
        const pid = await readPid(relativePath);
        if (pid !== undefined) trackedProcesses.add(pid);
      }
    };

    let timeoutCleanupPromise: Promise<void> | undefined;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    let readinessPoll: ReturnType<typeof setInterval> | undefined;
    let readinessTimeout: ReturnType<typeof setTimeout> | undefined;
    let readinessError: unknown;
    let timeoutStarted = false;

    const triggerTimeout = (): void => {
      if (settled || timeoutCleanupPromise) return;
      timedOut = true;
      timeoutCleanupPromise = (async () => {
        try {
          await registerConfiguredProcesses();
        } finally {
          await processHandle.terminate();
        }
      })();
      void timeoutCleanupPromise.catch(() => undefined);
    };

    const armTimeout = (): void => {
      if (timeoutStarted || settled) return;
      timeoutStarted = true;
      timeout = setTimeout(triggerTimeout, options.timeoutMs ?? 30_000);
    };

    if (options.readinessFile) {
      const readinessPath = projectPath(projectDir, options.readinessFile);
      const readinessDeadline =
        Date.now() + (options.readinessTimeoutMs ?? options.timeoutMs ?? 30_000);
      readinessPoll = setInterval(() => {
        void access(readinessPath)
          .then(() => {
            if (readinessPoll) clearInterval(readinessPoll);
            if (readinessTimeout) clearTimeout(readinessTimeout);
            armTimeout();
          })
          .catch((error) => {
            if (error instanceof Error && "code" in error && error.code === "ENOENT") return;
            readinessError = error;
            if (readinessPoll) clearInterval(readinessPoll);
            if (readinessTimeout) clearTimeout(readinessTimeout);
            triggerTimeout();
          });
      }, 25);
      readinessTimeout = setTimeout(() => {
        if (readinessPoll) clearInterval(readinessPoll);
        readinessError = new Error(
          `fixture readiness file did not appear before timeout: ${options.readinessFile}`,
        );
        triggerTimeout();
      }, Math.max(0, readinessDeadline - Date.now()));
    } else {
      armTimeout();
    }

    const result = await completion;
    if (timeout) clearTimeout(timeout);
    if (readinessPoll) clearInterval(readinessPoll);
    if (readinessTimeout) clearTimeout(readinessTimeout);
    if (timeoutCleanupPromise) await timeoutCleanupPromise;
    if (readinessError) throw readinessError;
    await processHandle.terminate();
    processes.delete(processHandle);
    return { ...result, timedOut };
  };

  return {
    binary,
    projectDir,
    rootDir,
    cleanup: async () => {
      if (cleanupPromise) return cleanupPromise;
      closed = true;
      const promise = (async () => {
        const errors: unknown[] = [];
        const owned = [...processes];
        for (const processHandle of owned) {
          try {
            await processHandle.registerTrackedProcesses();
            await processHandle.terminate();
          } catch (error) {
            errors.push(error);
          }
        }
        for (const pid of trackedProcessGroups) {
          try {
            if (isProcessGroupAlive(pid)) {
              sendProcessGroupSignal(pid, "SIGTERM");
              if (!(await waitForProcessGroupExit(pid, TRACKED_PROCESS_GRACE_MS))) {
                sendProcessGroupSignal(pid, "SIGKILL");
                if (!(await waitForProcessGroupExit(pid, FORCE_GRACE_MS))) {
                  errors.push(new Error(`tracked process group survived cleanup: ${pid}`));
                }
              }
            }
          } catch (error) {
            errors.push(error);
          }
        }
        for (const pid of trackedProcesses) {
          try {
            if (isProcessAlive(pid)) {
              sendProcessSignal(pid, "SIGTERM");
              if (!(await waitForProcessExit(pid, TRACKED_PROCESS_GRACE_MS))) {
                sendProcessSignal(pid, "SIGKILL");
                if (!(await waitForProcessExit(pid, FORCE_GRACE_MS))) {
                  errors.push(new Error(`tracked process survived cleanup: ${pid}`));
                }
              }
            }
          } catch (error) {
            errors.push(error);
          }
        }
        try {
          await rm(rootDir, { force: true, recursive: true });
        } catch (error) {
          errors.push(error);
        }
        try {
          await access(rootDir);
          errors.push(new Error(`cuenv e2e workspace survived cleanup: ${rootDir}`));
        } catch (error) {
          if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) {
            errors.push(error);
          }
        }
        if (errors.length > 0) {
          throw new AggregateError(errors, "cuenv e2e fixture cleanup failed");
        }
      })();
      cleanupPromise = promise;
      try {
        await promise;
      } catch (error) {
        if (cleanupPromise === promise) {
          cleanupPromise = undefined;
        }
        throw error;
      }
    },
    exists: async (relativePath) => {
      try {
        await access(projectPath(projectDir, relativePath));
        return true;
      } catch (error) {
        if (error instanceof Error && "code" in error && error.code === "ENOENT") {
          return false;
        }
        throw error;
      }
    },
    read: (relativePath) => readFile(projectPath(projectDir, relativePath), "utf8"),
    run,
    trackProcess: (pid) => {
      if (!Number.isInteger(pid) || pid <= 0) {
        throw new Error(`invalid process id: ${pid}`);
      }
      trackedProcesses.add(pid);
    },
    trackProcessGroup: (pid) => {
      if (!Number.isInteger(pid) || pid <= 0) {
        throw new Error(`invalid process group id: ${pid}`);
      }
      trackedProcessGroups.add(pid);
    },
    waitForProcessExit: (pid, timeoutMs = TRACKED_PROCESS_GRACE_MS) =>
      waitForProcessExit(pid, timeoutMs),
    waitForProcessGroupExit: (pid, timeoutMs = TRACKED_PROCESS_GRACE_MS) =>
      waitForProcessGroupExit(pid, timeoutMs),
    writeEnv: (content) => writeFile(join(projectDir, "env.cue"), content),
  };
}

export const test = base.extend<{ workspace: CuenvWorkspace }>({
  workspace: async (_fixtures, use) => {
    const workspace = await createWorkspace();
    try {
      await use(workspace);
    } finally {
      await workspace.cleanup();
    }
  },
});
