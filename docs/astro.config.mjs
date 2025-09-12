// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// https://astro.build/config
export default defineConfig({
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
					label: 'Getting Started',
					items: [
						{ label: 'Overview', slug: 'index' },
						{ label: 'Quick Start', slug: 'quick-start' },
						{ label: 'Installation', slug: 'installation' },
						{ label: 'Configuration', slug: 'configuration' },
					],
				},
				{
					label: 'Core Components',
					items: [
						{ label: 'CUE Engine', slug: 'cuengine' },
						{ label: 'Core Library', slug: 'cuenv-core' },
						{ label: 'CLI Tool', slug: 'cuenv-cli' },
					],
				},
				{
					label: 'Guides',
					items: [
						{ label: 'Task Orchestration', slug: 'tasks' },
						{ label: 'Typed Environments', slug: 'environments' },
						{ label: 'Secret Management', slug: 'secrets' },
						{ label: 'Nix Integration', slug: 'nix-integration' },
					],
				},
				{
					label: 'Reference',
					items: [
						{ label: 'API Reference', slug: 'api-reference' },
						{ label: 'Configuration Schema', slug: 'configuration-schema' },
						{ label: 'Examples', slug: 'examples' },
					],
				},
				{
					label: 'Development',
					items: [
						{ label: 'Contributing', slug: 'contributing' },
						{ label: 'Architecture', slug: 'architecture' },
						{ label: 'Development Setup', slug: 'development' },
						{ label: 'Troubleshooting', slug: 'troubleshooting' },
					],
				},
			],
		}),
	],
});
