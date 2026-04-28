import type {ReactNode} from 'react';
import clsx from 'clsx';
import Heading from '@theme/Heading';
import styles from './styles.module.css';

// SVG icons replace the prior 🏛️ ⛓️ 🌏 emoji set. Editorial brands
// don't use OS-rendered emoji as decoration — every glyph would render
// differently across mac / windows / linux / android, breaking visual
// consistency with the chain landing's icon vocabulary.

type FeatureItem = {
  title: string;
  icon: ReactNode;
  description: ReactNode;
};

const Pillar = (
  <svg viewBox="0 0 64 64" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M8 56 L56 56" />
    <path d="M12 56 L12 18" />
    <path d="M20 56 L20 18" />
    <path d="M32 56 L32 18" />
    <path d="M44 56 L44 18" />
    <path d="M52 56 L52 18" />
    <path d="M6 18 L58 18" />
    <path d="M6 18 L32 6 L58 18" />
  </svg>
);

const Diamond = (
  <svg viewBox="0 0 64 64" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <polygon points="32,6 58,32 32,58 6,32" />
    <polygon points="32,18 46,32 32,46 18,32" fill="currentColor" stroke="none" opacity="0.7" />
    <circle cx="32" cy="6" r="2" fill="currentColor" stroke="none" />
    <circle cx="58" cy="32" r="2" fill="currentColor" stroke="none" />
    <circle cx="32" cy="58" r="2" fill="currentColor" stroke="none" />
    <circle cx="6" cy="32" r="2" fill="currentColor" stroke="none" />
  </svg>
);

const Globe = (
  <svg viewBox="0 0 64 64" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="32" cy="32" r="26" />
    <ellipse cx="32" cy="32" rx="14" ry="26" />
    <path d="M6 32 L58 32" />
    <path d="M11 17 Q32 24 53 17" />
    <path d="M11 47 Q32 40 53 47" />
  </svg>
);

const FeatureList: FeatureItem[] = [
  {
    title: 'Real Asset Infrastructure',
    icon: Pillar,
    description: (
      <>
        EVM-native L1 with Solidity smart contracts (revm 37). Built ground-up
        for real-world asset use cases. EIP-1559 fee market, ERC-20 / ERC-721 /
        SRC-721 token standards specified, MetaMask + hardhat + ethers.js ready
        out of the box.
      </>
    ),
  },
  {
    title: 'Bitcoin Discipline',
    icon: Diamond,
    description: (
      <>
        Fixed 315M SRX cap. 4-year halving schedule, BTC-parity. No governance
        inflation, no validator reward debasement. Voyager DPoS+BFT consensus with
        1-second finality. Treasury-escrow rewards with permissionless ClaimRewards.
      </>
    ),
  },
  {
    title: 'Indonesia First, Global Next',
    icon: Globe,
    description: (
      <>
        Financial infrastructure for the real economy — Indonesia first, then the
        world. 1-second blocks, 0.0001 SRX min fee, 5,000 tx per block. Built for
        Southeast Asia&apos;s 600 million people.
      </>
    ),
  },
];

function Feature({title, icon, description}: FeatureItem) {
  return (
    <div className={clsx('col col--4')}>
      <div className="text--center">
        <div className={styles.featureIcon}>{icon}</div>
      </div>
      <div className="text--center padding-horiz--md">
        <Heading as="h3" className={styles.featureTitle}>{title}</Heading>
        <p className={styles.featureDesc}>{description}</p>
      </div>
    </div>
  );
}

export default function HomepageFeatures(): ReactNode {
  return (
    <section className={styles.features}>
      <div className="container">
        <div className="row">
          {FeatureList.map((props, idx) => (
            <Feature key={idx} {...props} />
          ))}
        </div>
      </div>
    </section>
  );
}
