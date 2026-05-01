import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'Sentrix Chain',
  tagline: 'Open source, EVM-compatible L1 built in Rust.',
  favicon: 'img/favicon.ico',

  future: {
    v4: true,
  },

  url: 'https://docs.sentrixchain.com',
  baseUrl: '/',

  organizationName: 'sentrix-labs',
  projectName: 'sentrix',

  onBrokenLinks: 'warn',
  onBrokenMarkdownLinks: 'warn',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          path: '../docs',
          // Skip files that are GitHub-specific (folder README) — Docusaurus
          // would otherwise try to render docs/README.md at the same route
          // as src/pages/index.tsx (the marketing homepage).
          exclude: ['README.md'],
          sidebarPath: './sidebars.ts',
          routeBasePath: '/',
          editUrl:
            'https://github.com/sentrix-labs/sentrix/tree/main/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/sentrix-social-card.png',
    colorMode: {
      // Sentrix is a dark-first brand — first paint stays dark even when
      // the OS prefers light. Toggle still works for users who want light.
      defaultMode: 'dark',
      respectPrefersColorScheme: false,
    },
    navbar: {
      title: 'Sentrix Chain',
      logo: {
        alt: 'Sentrix Chain Logo',
        src: 'img/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          href: 'https://github.com/sentrix-labs/sentrix',
          label: 'GitHub',
          position: 'right',
        },
        {
          href: 'https://sentrixchain.com',
          label: 'sentrixchain.com',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Build',
          items: [
            {label: 'Quickstart', to: '/operations/DEVELOPER_QUICKSTART'},
            {label: 'Smart Contracts', to: '/operations/SMART_CONTRACT_GUIDE'},
            {label: 'API Reference', to: '/operations/API_ENDPOINTS'},
            {label: 'MetaMask Setup', to: '/operations/METAMASK'},
          ],
        },
        {
          title: 'Operate',
          items: [
            {label: 'Validator Guide', to: '/operations/VALIDATOR_GUIDE'},
            {label: 'Networks', to: '/operations/NETWORKS'},
            {label: 'Monitoring', to: '/operations/MONITORING'},
            {label: 'Emergency Rollback', to: '/operations/EMERGENCY_ROLLBACK'},
          ],
        },
        {
          title: 'Tokenomics',
          items: [
            {label: 'SRX', to: '/tokenomics/SRX'},
            {label: 'Token Standards', to: '/tokenomics/TOKEN_STANDARDS'},
            {label: 'Staking', to: '/tokenomics/STAKING'},
          ],
        },
        {
          title: 'Contact',
          items: [
            {label: 'Builders', href: 'mailto:builders@sentrixchain.com'},
            {label: 'Grants', href: 'mailto:grants@sentrixchain.com'},
            {label: 'Partners', href: 'mailto:partners@sentriscloud.com'},
            {label: 'Press', href: 'mailto:press@sentriscloud.com'},
            {label: 'Security', href: 'mailto:security@sentrixchain.com'},
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} Sentrix Labs.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['rust', 'solidity', 'bash', 'json', 'toml'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
