import type {ReactNode} from 'react';
import clsx from 'clsx';
import Heading from '@theme/Heading';
import styles from './styles.module.css';

type FeatureItem = {
  title: string;
  icon: string;
  description: ReactNode;
};

const FeatureList: FeatureItem[] = [
  {
    title: 'Real Asset Infrastructure',
    icon: '🏛️',
    description: (
      <>
        EVM-native L1 with Solidity smart contracts (revm 37). Built for real-world
        assets — invoices, receivables, gold, real estate. Sourcify verification,
        canonical contract suite, and token standards (ERC-20, ERC-721, SRC-721)
        production-ready.
      </>
    ),
  },
  {
    title: 'Bitcoin Discipline',
    icon: '⛓️',
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
    icon: '🌏',
    description: (
      <>
        Financial infrastructure for the real economy — Indonesia first, then the
        world. 1-second blocks, 0.0001 SRX min fee, 5,000 tx per block. Built for
        Southeast Asia's 600 million people. RWA-ready from day one.
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
        <Heading as="h3">{title}</Heading>
        <p>{description}</p>
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
