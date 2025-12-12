# cuenv documentation site

This directory contains the cuenv documentation site built with [Astro](https://astro.build/) + [Starlight](https://starlight.astro.build/).

## Local development

From this directory:

```bash
bun install
bun run dev
```

Build and preview:

```bash
bun run build
bun run preview
```

## Where docs live

Starlight content is under:

- `docs/src/content/docs/`

The site is organized using the [Diátaxis framework](https://diataxis.fr/):

- `docs/src/content/docs/tutorials/`
- `docs/src/content/docs/how-to/`
- `docs/src/content/docs/reference/`
- `docs/src/content/docs/explanation/`
- `docs/src/content/docs/decisions/` (ADRs + RFCs)

## Writing guidelines

- **One page, one purpose**: don’t mix tutorial steps, reference tables, and architectural rationale in the same page.
- **Prefer links over duplication**: if a how-to needs command/flag details, link to Reference.
- **Keep examples runnable**: snippets should work as written (or clearly mark placeholders).
