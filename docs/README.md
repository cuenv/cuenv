# cuenv Documentation

This directory contains the source files for the cuenv documentation website, built with [Antora](https://antora.org/) and written in [AsciiDoc](https://asciidoctor.org/).

## Building Documentation

### Prerequisites

- Node.js 16+ 
- npm

### Quick Start

```bash
# Install dependencies
npm install

# Build documentation
npm run docs:build

# Serve locally
npm run docs:serve
```

The documentation will be available at http://localhost:8080

### Development Workflow

```bash
# Build and serve in one command
npm run docs:dev
```

## Structure

```
docs/
├── antora.yml              # Antora component descriptor
├── modules/
│   └── ROOT/
│       ├── nav.adoc        # Site navigation
│       ├── pages/          # Documentation pages
│       ├── assets/         # Images and other assets
│       └── examples/       # Code examples
└── README.md              # This file
```

## Writing Guidelines

- Use AsciiDoc format for all documentation
- Follow the existing page structure and navigation
- Include code examples where appropriate
- Cross-reference related pages using `xref:`
- Test all code examples before committing

## Building Process

The documentation is built using Antora, which:

1. Reads the `antora-playbook.yml` configuration
2. Processes AsciiDoc files in `docs/modules/ROOT/pages/`
3. Applies the UI theme
4. Generates a static website in `build/site/`

## Deployment

Documentation is automatically built and deployed via GitHub Actions:

- **Pull Requests**: Documentation is built and artifacts are saved
- **Main Branch**: Documentation is built and deployed to GitHub Pages

## Contributing

1. Edit or add AsciiDoc files in `docs/modules/ROOT/pages/`
2. Update navigation in `docs/modules/ROOT/nav.adoc` if adding new pages
3. Test locally with `npm run docs:build`
4. Submit a pull request

For detailed contributing guidelines, see the main [Contributing Guide](../CONTRIBUTING.md).