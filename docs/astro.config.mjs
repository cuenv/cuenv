// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import cloudflare from '@astrojs/cloudflare';

// https://astro.build/config
export default defineConfig({
	site: 'https://cuenv.dev',
	output: 'server',
	adapter: cloudflare({
		imageService: 'cloudflare'
	}),
	integrations: [
		starlight({
			title: 'cuenv Documentation',
			description: 'A modern application build toolchain with typed environments and CUE-powered task orchestration',
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/cuenv/cuenv' },
				{ icon: 'github', label: 'Discussions', href: 'https://github.com/cuenv/cuenv/discussions' }
			],
			editLink: {
				baseUrl: 'https://github.com/cuenv/cuenv/edit/main/docs/',
			},
			sidebar: [
				{
					label: 'Tutorials',
					items: [
						{ label: 'Tutorials', slug: 'tutorials' },
						{ label: 'Your first cuenv project', slug: 'tutorials/first-project' },
						{ label: 'Use cuenv in a monorepo', slug: 'tutorials/monorepo' },
					],
				},
				{
					label: 'How-to guides',
					items: [
						{ label: 'How-to guides', slug: 'how-to' },
						{ label: 'Install cuenv', slug: 'how-to/install' },
						{ label: 'Editor setup', slug: 'how-to/editor-setup' },
						{ label: 'Configure a project', slug: 'how-to/configure-a-project' },
						{ label: 'Run tasks', slug: 'how-to/run-tasks' },
						{ label: 'Cubes (Codegen)', slug: 'how-to/cubes' },
						{ label: 'Typed environments', slug: 'how-to/typed-environments' },
						{ label: 'Secrets', slug: 'how-to/secrets' },
						{ label: 'Nix', slug: 'how-to/nix' },
						{ label: 'Tools', slug: 'how-to/tools' },
						{ label: 'Troubleshooting', slug: 'how-to/troubleshooting' },
						{ label: 'Develop cuenv', slug: 'how-to/develop-cuenv' },
						{ label: 'Contribute', slug: 'how-to/contribute' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'Reference', slug: 'reference' },
						{ label: 'CLI', slug: 'reference/cli' },
						{ label: 'CUE schema', slug: 'reference/cue-schema' },
						{ label: 'Rust API', slug: 'reference/rust-api' },
						{ label: 'Examples', slug: 'reference/examples' },
					],
				},
				{
					label: 'Explanation',
					items: [
						{ label: 'Explanation', slug: 'explanation' },
						{ label: 'Architecture', slug: 'explanation/architecture' },
						{ label: 'cuengine', slug: 'explanation/cuengine' },
						{ label: 'cuenv-codeowners', slug: 'explanation/cuenv-codeowners' },
						{ label: 'cuenv-events', slug: 'explanation/cuenv-events' },
						{ label: 'cuenv-cubes (Codegen)', slug: 'explanation/cuenv-cubes' },
						{ label: 'cuenv-ignore', slug: 'explanation/cuenv-ignore' },
						{ label: 'cuenv-workspaces', slug: 'explanation/cuenv-workspaces' },
						{ label: 'Dagger backend', slug: 'explanation/dagger-backend' },
						{ label: 'Roadmap', slug: 'explanation/roadmap' },
						{ label: 'Decisions (ADRs & RFCs)', slug: 'decisions' },
					],
				},
				{
					label: 'Home',
					items: [
						{ label: 'Overview', slug: 'index' },
					],
				},
			],
		}),
	],
});
