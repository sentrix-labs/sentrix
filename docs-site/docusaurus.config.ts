import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

const config: Config = {
  title: 'Sentrix Chain',
  tagline: 'Where real assets live.',
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
          sidebarPath: './sidebars.ts',
          routeBasePath: '/',
          editUrl:
            'https://github.com/sentrix-labs/sentrix/tree/main/docs-site/',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/sentrix-social-card.jpg',
    colorMode: {
      respectPrefersColorScheme: true,
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
      copyright: `Copyright © ${new Date().getFullYear()} SentrisCloud. Where real assets live.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['rust', 'solidity', 'bash', 'json', 'toml'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
