# Sentrix — sentrixchain.com Hero Copy

Hero section for `sentrixchain.com` (chain & developer audience). For business/products audience hero, see `founder-private/marketing/SENTRISCLOUD_HERO_COPY.md`.

---

## Headline (h1)

```
Where real assets live.
```

## Sub-headline

```
Sentrix is the financial infrastructure for the real economy —
starting with Indonesia. We bring real-world assets on-chain
with Bitcoin's monetary discipline (fixed 315M supply, 4-year
halving) and Ethereum's programmability (EVM-native, Solidity-
ready) — built for Southeast Asia's 600 million people first,
then the world.
```

## CTAs (3 buttons)

```
Primary:    Read the whitepaper       → /whitepaper.pdf (or GitHub link)
Secondary:  Run a node                → /docs/run-a-node
Tertiary:   Build a dApp              → /docs/build
```

## Trust strip (live network stats — pull from RPC)

```
Mainnet     |  Block Time  |  Validators  |  Max Supply  |  Consensus
Live h=...  |  1 second    |  4 active    |  315M SRX    |  Voyager DPoS+BFT
```

JS to populate:
```js
// Pull from rpc.sentrixchain.com/chain/info
const stats = await fetch('https://rpc.sentrixchain.com/chain/info').then(r => r.json());
// { height, active_validators, max_supply_srx, consensus_mode }
```

## Tech-stack pill row (under hero, optional)

```
Built in Rust · EVM-compatible · MetaMask-ready · 1s blocks · Production since April 2026
```

## Quick-start strip (bottom of hero, for builders)

```
Devs:       npm install ethers           Connect to chain 7119
            const provider = new ethers.JsonRpcProvider(
              "https://rpc.sentrixchain.com/rpc"
            );

Validators: github.com/sentrix-labs/sentrix#run-a-validator
            Min self-stake: 15,000 SRX
```

---

## Footer contact (sentrixchain.com bottom)

```
Builders:  builders@sentrixchain.com
Grants:    grants@sentrixchain.com
Validators: validators@sentrixchain.com
Security disclosure: security@sentrixchain.com
Network abuse: abuse@sentrixchain.com
```

---

## Voice & tone notes

- **Direct, technical, confident.** Audience = developers + validators + technical RWA partners.
- **Avoid marketing fluff.** No "revolutionary", "next-gen", "game-changing".
- **Concrete numbers.** Always anchor with specifics (315M, 4-year, 1s, chain 7119).
- **English primary.** Add Indonesian variants (`/id/`) only after primary has v1 traction.
- **No specific RWA verticals** in hero (per BRAND_POSITIONING.md). Generic "real-world assets". Add specific verticals only when signed partnerships materialize.

---

## Source positioning (do not edit here — see master)

Master: [`founder-private/BRAND_POSITIONING.md`](../../../founder-private/BRAND_POSITIONING.md) (or memory `project_sentrix_positioning.md`)
