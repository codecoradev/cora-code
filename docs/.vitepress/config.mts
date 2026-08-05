import { createConfig } from '@codecora/theme/vitepress/config'

export default createConfig({
  product: 'cora',
  title: 'Cora',
  description: 'AI-Powered Code Review CLI — BYOK, zero config, runs in your terminal',
  accent: 'green',
  repo: 'cora-code',
  head: [
    ['meta', { property: 'og:title', content: 'Cora — AI Code Review CLI' }],
    ['meta', { property: 'og:description', content: 'BYOK, zero config, runs in your terminal' }],
  ],
  lastUpdated: true,
  ignoreDeadLinks: [/^https?:\/\/localhost/],
  sidebar: {
    '/': [
      {
        text: 'Getting Started',
        items: [
          { text: 'Quick Start', link: '/getting-started' },
          { text: 'Installation', link: '/installation' },
        ],
      },
      {
        text: 'Guides',
        items: [
          { text: 'Usage', link: '/usage' },
          { text: 'Configuration', link: '/configuration' },
          { text: 'Code Intelligence', link: '/code-intelligence' },
          { text: 'Providers', link: '/providers' },
          { text: 'CLI Reference', link: '/cli-reference' },
          { text: 'Examples', link: '/examples' },
        ],
      },
      {
        text: 'Project',
        items: [
          { text: 'Changelog', link: '/changelog' },
          { text: 'Roadmap', link: '/roadmap' },
        ],
      },
    ],
  },
})
