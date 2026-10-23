import type { E2EConfig } from "e2e";

export default {
  projectId: "cuenv-cli-acceptance",
  tests: "tests/e2e/**/*.e2e.ts",
  targets: [
    {
      name: "cli",
      platform: process.platform,
    },
  ],
  timeout: 60_000,
  cleanupTimeout: 30_000,
  retries: 0,
  workers: 2,
  cache: "off",
  reporters: ["list", "junit", "markdown"],
  output: ".e2e",
} satisfies E2EConfig;
