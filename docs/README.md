# cuenv documentation site

This directory contains the cuenv documentation site, published at
[cuenv.dev](https://cuenv.dev). It is built with
[Nimbus](https://nimbus-docs.com/) (`@cloudflare/nimbus-docs`) on
[Astro](https://astro.build/) and deployed as static assets to Cloudflare
Workers.

## Local development

Dependencies are installed from the repository root (the docs site is a bun
workspace member). From this directory:

```bash
bun install
bun run dev
```

Build, check, and preview:

```bash
bun run build      # static site in dist/, including the Pagefind search index
bun run check      # nimbus-docs check: config, structure, links, frontmatter
bun run typecheck  # astro check
bun run preview
```

Search only works against a build (`bun run build` then `bun run preview`), not
`bun run dev`.

In CI, `cuenv task docs.build` builds the site and `cuenv task docs.deploy`
publishes it with Wrangler (`wrangler.jsonc`).

Every pull request that touches the docs also runs the `docs-preview` pipeline.
Its `docs.preview` task uploads the build as a
[Worker Preview](https://developers.cloudflare.com/workers/previews/) named
`pr-<number>` (`wrangler preview`, Wrangler 4.135 or later). Its `workers.dev`
URL is captured into the pipeline's "Docs preview" annotation, which shows in
the check run, the job summary and cuenv's PR comment. Each push updates the
same Preview.
Production on cuenv.dev is untouched. Previews are not deleted when a PR closes
yet; remove one with `bunx wrangler preview delete --name pr-<number>` from
`docs/`. PRs from forks get no preview, because they cannot read the 1Password
service account secret.

## Where docs live

Pages are under `docs/src/content/docs/`. The folder tree is the URL structure,
and the sidebar order and labels are set in `astro.config.ts`.

The site is organized using the [Diátaxis framework](https://diataxis.fr/):

- `docs/src/content/docs/tutorials/`
- `docs/src/content/docs/how-to/`
- `docs/src/content/docs/reference/`
- `docs/src/content/docs/explanation/`
- `docs/src/content/docs/decisions/` (ADRs + RFCs)

Directories outside `src/` (`design/`, `releases/`, `superpowers/`) are
repository notes and are not published.

## Markdown and MDX

- Use `.mdx` for any page with components (`<Aside>`, `<Card>`, `<CardGrid>`,
  `<Steps>`, `<Tabs>`, `<LinkButton>`) or `:::note` / `:::tip` / `:::caution`
  admonitions. Plain `.md` renders neither.
- Components registered in `src/components.ts` work in MDX without an import.
  Card icons use [Phosphor](https://phosphoricons.com/) names, e.g.
  `icon="ph:arrow-right"`.
- In `.mdx`, `{` and `<` in prose are parsed as expressions and tags. Wrap them
  in backticks.
- Link to other pages with root-relative paths (`/how-to/install/`). Link to
  repository files with full GitHub URLs.

## Upgrading Nimbus

The site's components, layouts, and pages are copied into `src/` and are ours
to edit; `nimbus.json` records which Nimbus templates they came from. To see
and pull upstream changes:

```bash
bunx nimbus-docs outdated
bunx nimbus-docs diff
bunx nimbus-docs migrate --dry-run
```

## Writing guidelines

- **One page, one purpose**: don’t mix tutorial steps, reference tables, and architectural rationale in the same page.
- **Prefer links over duplication**: if a how-to needs command/flag details, link to Reference.
- **Keep examples runnable**: snippets should work as written (or clearly mark placeholders).
