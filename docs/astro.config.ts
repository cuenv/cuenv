import { defineConfig } from "astro/config";
import tailwindcss from "@tailwindcss/vite";
import nimbus, {
  defineConfig as defineNimbusConfig,
} from "@cloudflare/nimbus-docs";
import { tableScroll } from "@cloudflare/nimbus-docs/markdown";

const nimbusConfig = defineNimbusConfig({
  site: "https://cuenv.dev",
  title: "cuenv",
  description:
    "A modern application build toolchain with typed environments and CUE-powered task orchestration",
  locale: "en",
  github: "https://github.com/cuenv/cuenv",
  editPattern: "https://github.com/cuenv/cuenv/edit/main/docs/{path}",
  socialImageAlt: "cuenv documentation",
  sidebar: {
    items: [
      { label: "Overview", link: "/" },
      {
        label: "Tutorials",
        items: [
          { label: "Tutorials", link: "/tutorials/" },
          { label: "Your first cuenv project", link: "/tutorials/first-project/" },
          { label: "Use cuenv in a monorepo", link: "/tutorials/monorepo/" },
        ],
      },
      {
        label: "How-to guides",
        items: [
          { label: "How-to guides", link: "/how-to/" },
          { label: "Install cuenv", link: "/how-to/install/" },
          { label: "Editor setup", link: "/how-to/editor-setup/" },
          { label: "Configure a project", link: "/how-to/configure-a-project/" },
          { label: "Run tasks", link: "/how-to/run-tasks/" },
          { label: "Run services", link: "/how-to/services/" },
          { label: "Manage infrastructure", link: "/how-to/infrastructure/" },
          { label: "Task scripts", link: "/how-to/task-scripts/" },
          { label: "CI", link: "/how-to/ci/" },
          { label: "Codegen", link: "/how-to/codegen/" },
          { label: "Formatters", link: "/how-to/formatters/" },
          { label: "Ignore files", link: "/how-to/ignore-files/" },
          { label: "CODEOWNERS", link: "/how-to/codeowners/" },
          { label: "Generate EditorConfig", link: "/how-to/editorconfig/" },
          { label: "Typed environments", link: "/how-to/typed-environments/" },
          { label: "Contexts", link: "/how-to/contexts/" },
          { label: "Secrets", link: "/how-to/secrets/" },
          { label: "Build container images", link: "/how-to/container-images/" },
          { label: "Nix", link: "/how-to/nix/" },
          { label: "Lockfiles", link: "/how-to/lockfiles/" },
          { label: "Manage releases", link: "/how-to/releases/" },
          { label: "Tools", link: "/how-to/tools/" },
          { label: "Choosing a runtime", link: "/how-to/runtimes/" },
          { label: "VCS dependencies", link: "/how-to/vcs-dependencies/" },
          { label: "Troubleshooting", link: "/how-to/troubleshooting/" },
          { label: "Develop cuenv", link: "/how-to/develop-cuenv/" },
          { label: "Contribute", link: "/how-to/contribute/" },
        ],
      },
      {
        label: "Reference",
        items: [
          { label: "Reference", link: "/reference/" },
          { label: "CLI", link: "/reference/cli/" },
          { label: "CUE schema", link: "/reference/cue-schema/" },
          { label: "Schema status", link: "/reference/schema/status/" },
          { label: "Rust API", link: "/reference/rust-api/" },
          { label: "Examples", link: "/reference/examples/" },
        ],
      },
      {
        label: "Agents",
        items: [
          { label: "Agent guides", link: "/agents/" },
          { label: "Schema-first workflow", link: "/agents/schema-first/" },
        ],
      },
      {
        label: "Explanation",
        items: [
          { label: "Explanation", link: "/explanation/" },
          { label: "Why cuenv", link: "/explanation/why-cuenv/" },
          { label: "Architecture", link: "/explanation/architecture/" },
          { label: "Cuetty", link: "/explanation/cuetty/" },
          { label: "Cuetty daily-driver roadmap", link: "/explanation/cuetty-daily-driver-roadmap/" },
          { label: "Cuetty M0 capability evidence", link: "/explanation/cuetty-m0-capability-report/" },
          { label: "Cuetty gap analysis", link: "/explanation/cuetty-gap-analysis/" },
          { label: "cuengine", link: "/explanation/cuengine/" },
          { label: "cuenv-codeowners", link: "/explanation/cuenv-codeowners/" },
          { label: "cuenv-events", link: "/explanation/cuenv-events/" },
          { label: "cuenv-codegen", link: "/explanation/cuenv-codegen/" },
          { label: "cuenv-formatters", link: "/explanation/cuenv-formatters/" },
          { label: "cuenv-ignore", link: "/explanation/cuenv-ignore/" },
          { label: "cuenv-workspaces", link: "/explanation/cuenv-workspaces/" },
          { label: "Roadmap", link: "/explanation/roadmap/" },
          { label: "Decisions (ADRs & RFCs)", link: "/decisions/" },
        ],
      },
    ],
  },
});

export default defineConfig({
  output: "static",
  // Tailwind v4 via its Vite plugin.
  vite: {
    plugins: [tailwindcss()],
  },
  // Hover-prefetch link targets so full-page navigations feel instant without
  // a client-side router.
  prefetch: {
    prefetchAll: true,
    defaultStrategy: "hover",
  },
  integrations: [
    nimbus(nimbusConfig, {
      rules: {
        "nimbus/frontmatter-shape": "error",
        "nimbus/internal-link": "error",
      },
      markdown: {
        // Wrap wide tables so they scroll instead of overflowing the page
        // (styled by `.nb-table-scroll` in src/styles/prose.css).
        hastPlugins: [tableScroll()],
      },
    }),
  ],
});
