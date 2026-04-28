# Sentrix Chain — Peta Jalan Indonesia

**Versi:** 1.0
**Tanggal:** 28 April 2026
**Bahasa:** Bahasa Indonesia (dokumen pertama dalam program i18n Sentrix)

> Dokumen ini menjelaskan posisi "Indonesia first" Sentrix Chain dan peta jalan ekosistem di pasar Indonesia. Untuk versi teknis lengkap dalam bahasa Inggris, lihat dokumen utama di [docs.sentrixchain.com](https://docs.sentrixchain.com).

---

## Tentang Sentrix Chain

Sentrix Chain adalah blockchain Layer-1 yang dibangun untuk **infrastruktur keuangan ekonomi nyata — Indonesia dulu, lalu dunia**. Kami fokus ke aset dunia nyata (RWA), sistem pembayaran, dan pondasi DeFi yang relevan untuk pasar negara berkembang, dimulai dari Indonesia.

**Spesifikasi teknis singkat:**

- **Chain ID:** 7119 (mainnet) / 7120 (testnet)
- **Token native:** SRX (8 desimal)
- **Konsensus:** DPoS + BFT (validator-based, instant finality)
- **Block time:** ~1 detik
- **Suplai maksimum:** 315.000.000 SRX (4-year halving model, ala Bitcoin)
- **EVM compatibility:** Penuh (kontrak Solidity bisa di-deploy)
- **Status:** Mainnet aktif sejak 25 April 2026

---

## Mengapa Indonesia dulu?

Pertanyaan yang sering muncul: kenapa tidak target Amerika atau Eropa dulu seperti project crypto pada umumnya? Jawaban kami:

### 1. Pasar yang underserved tapi mature

Indonesia punya 270+ juta penduduk dengan adopsi mobile-first yang tinggi. Crypto user base di Indonesia sudah melewati 18 juta orang per regulasi Bappebti per akhir 2025. Namun:

- Hampir semua chain populer (Ethereum, BSC, Solana, Polygon) **dirancang untuk pasar Barat** — UI Inggris, pricing dalam USD, dukungan komunitas tidak menjangkau pengguna Indonesia
- Native local stablecoin / IDR-pair sangat terbatas
- DEX dan DeFi protokol jarang yang punya dukungan Bahasa Indonesia memadai
- Edukasi blockchain dalam Bahasa Indonesia masih scattered, tidak terstruktur

Sentrix mengisi gap ini dengan dokumen native Bahasa Indonesia (mulai dengan ini), partnership dengan CEX lokal, dan UI yang dirancang untuk pengguna Indonesia.

### 2. Regulasi yang clear dan konstruktif

Bappebti dan OJK telah mengeluarkan kerangka regulasi crypto yang relatif clear sejak 2024–2025. Sentrix Chain dibangun **dengan compliance dalam pikiran** sejak awal:

- Tidak menargetkan AS atau wilayah dengan regulasi ambigu
- Posisi RWA-first sejalan dengan arahan Bappebti untuk crypto-as-utility
- Treasury governance multisig (SentrixSafe) memberikan trail audit yang regulator-friendly
- Roadmap listing diutamakan ke CEX bersertifikat Bappebti (Tokocrypto, Pintu, Indodax)

### 3. Strategic depth via local network

Founder Sentrix (Satya Kwok) berbasis di Indonesia. Tim core, advisor planning, dan partnership pipeline berakar di komunitas Indonesia. Ini bukan project asing yang "menambahkan dukungan Indonesia" — ini project Indonesia dari awal.

---

## Peta Jalan — Indonesia Spesifik

Roadmap di bawah ini fokus ke milestone yang relevan untuk pasar Indonesia. Untuk roadmap teknis chain secara menyeluruh, lihat dokumen Roadmap di [`docs/roadmap/`](../roadmap/PHASE1) (Phase 1, 2, 3).

### Q2 2026 (April – Juni) — Foundation & Visibility

| Milestone | Status | Catatan |
|---|---|---|
| Mainnet Sentrix Chain stabil | ✅ Selesai 25 April 2026 | 4 validator aktif, EVM live, V4 reward distribution aktif |
| Submit ke Chainlist.org | ✅ PR diajukan (`ethereum-lists/chains#8266`) | Menunggu maintainer review |
| Submit ke CoinGecko + CoinMarketCap | ⏳ Aplikasi sedang disusun | Target launch listing Q2 |
| Audit eksternal — engagement firma audit | ⏳ Q2 target | 6–8 minggu audit + remediation window |
| Verifikasi kontrak self-host (Sourcify-equivalent) | ⏳ Q2 target | Untuk dApp builder dapat verify bytecode ↔ source |
| Faucet testnet SRX (10M tSRX per claim) | ✅ Live | https://faucet.sentrixchain.com (testnet) |
| Block explorer | ✅ Live | https://scan.sentrixchain.com |

### Q3 2026 (Juli – September) — Indonesian Ecosystem Bootstrap

| Milestone | Catatan |
|---|---|
| **Airdrop Phase 1 — Testnet Heroes** (1.000.000 SRX) | Setelah Chainlist listing approved. Detail di [`AIRDROP_MECHANICS.md`](../tokenomics/AIRDROP_MECHANICS.md) |
| **Airdrop Phase 2 — Quest Campaign** (1.000.000 SRX) | Galxe / Zealy-style integration, partnership platform Indonesia |
| **DEX launch (canonical AMM)** | Uniswap V2-fork atau equivalen, bootstrap likuiditas dari Strategic Reserve (1.5M SRX) |
| **Engagement Tokocrypto + Pintu** | Listing CEX Indonesia tier-1, Bappebti-licensed |
| **SDK packaging** (`@sentrix/sdk-js`, `sentrix-sdk-rs`) | Untuk dApp developer Indonesia bisa integrate cepat |
| **SentrixSafe migrasi 1-of-1 → 3-of-5** | Governance multisig dengan 3 dari 5 signer (founder, founder backup, 2 advisor independen, security council seat) |
| **Founder vesting contract deploy** | Lock 21M SRX founder allocation on-chain (saat ini social commitment, akan jadi enforced contract) |
| **Bahasa Indonesia content expansion** | Translasi docs utama (validator guide, smart contract tutorial, faucet, claim airdrop) |

### Q4 2026 (Oktober – Desember) — Real Economy Integration

| Milestone | Catatan |
|---|---|
| **Airdrop Phase 3 — Activity Rewards** (800.000 SRX) | Reward untuk active mainnet wallets, snapshot mainnet |
| **Airdrop Phase 4 — Validator Delegators** (700.000 SRX) | Pro-rata ke delegator pada active validator set |
| **External validator onboarding** | Saat ini 4 validator semua Foundation-operated. Target: ≥10 validator dengan operator independen |
| **Indodax + 1 CEX Indonesia tambahan listing** | Tier-1 Indonesia CEX coverage |
| **First RWA pilot integration** | Aset dunia nyata pertama on-chain (kategori belum diumumkan; akan di-publish saat partnership signed) |
| **Native event/log system + GraphQL indexer** | Infrastruktur dApp-level — analytics, NFT marketplace listings, dApp discovery |
| **Komunitas builder Indonesia — bootcamp + grants** | Program dev edukasi + grant pool dari Ecosystem Fund |

### 2027+ — Skala & Decentralisasi

| Milestone | Catatan |
|---|---|
| **Airdrop Phase 5 — Retroactive Builders** (1.500.000 SRX) | Committee-reviewed: dApp deployers, audit contributors, ecosystem PRs |
| **Tier-2 CEX international** | Gate.io, MEXC, KuCoin tier (post DEX TVL > $1M) |
| **On-chain governance untuk protocol upgrade** | Saat ini upgrade dilakukan via koordinasi operator. Q4 2027+ target: voting-based governance |
| **Decentralized treasury (DAO-style)** | Replace Foundation-coordinated multisig dengan governance token-based decision making |
| **Tier-3 CEX (Binance, Coinbase) — realistik post-traction** | Setelah TVL meaningful + daily active address sustainable + regulator clarity |

---

## Komunitas Indonesia — Cara Berpartisipasi

### Untuk pengguna umum

1. **Cobain testnet:** Klaim 10M tSRX di [https://faucet.sentrixchain.com](https://faucet.sentrixchain.com) (chain ID 7120 di Metamask)
2. **Cek block explorer:** [https://scan.sentrixchain.com](https://scan.sentrixchain.com) — lihat transaksi, validator, kontrak
3. **Follow channel resmi:** Telegram, Twitter/X (akun resmi akan dipublish menjelang Phase 1 airdrop)
4. **Eligibility airdrop Phase 1** (Testnet Heroes): Lakukan minimum 50 tx di testnet, deploy minimum 1 kontrak, atau jadi validator testnet selama ≥7 hari sebelum snapshot height (target 400.000)

### Untuk developer

1. **Read public docs:** [docs.sentrixchain.com](https://docs.sentrixchain.com) — start dari `intro.md` dan `DEVELOPER_QUICKSTART.md`
2. **Local development:** Chain Sentrix EVM-compatible — kalau familiar dengan Solidity + Hardhat / Foundry, langsung bisa deploy
3. **Canonical contracts available:**
   - **WSRX** (wrapped SRX, untuk DEX/DeFi): `0x4693b113e523A196d9579333c4ab8358e2656553`
   - **Multicall3** (batch RPC calls): `0xFd4b34b5763f54a580a0d9f7997A2A993ef9ceE9`
   - **TokenFactory** (deploy SRC-20 dengan 1 tx): `0xc753199b723649ab92c6db8A45F158921CFDEe49`
4. **Grants & retroactive rewards:** Ship aplikasi yang nyata (DEX, NFT, RWA, payment) → eligible untuk Phase 5 (Retroactive Builders, 1.5M SRX pool)

### Untuk validator

1. **Saat ini:** Foundation-operated 4 validator. External onboarding mulai Q4 2026.
2. **Persyaratan teknis (akan dipublish):** minimum self-stake, hardware spec, uptime SLA, networking tier
3. **Reward economics:** V4 distribution — block reward + tx fee revenue, pro-rata berdasarkan stake

### Untuk content creator / educator

1. **Bahasa Indonesia gap:** kami butuh tutorial, video, threads, deep-dive Bahasa Indonesia tentang Sentrix
2. **Phase 5 retroactive eligible:** content creator dengan output verified + non-trivial → committee review untuk grant alokasi
3. **Strategic Reserve dukungan partnership:** untuk creator dengan reach signifikan, partnership formal possible (Q3+ post-multisig migration)

---

## Kontak

- **Website:** [https://sentrixchain.com](https://sentrixchain.com)
- **Docs:** [https://docs.sentrixchain.com](https://docs.sentrixchain.com)
- **Faucet:** [https://faucet.sentrixchain.com](https://faucet.sentrixchain.com)
- **Block explorer:** [https://scan.sentrixchain.com](https://scan.sentrixchain.com)
- **GitHub:** [github.com/sentrix-labs](https://github.com/sentrix-labs)
- **Security disclosures:** `security@sentriscloud.com` (lihat [SECURITY.md](https://github.com/sentrix-labs/sentrix/blob/main/SECURITY.md))
- **General contact:** Akan dipublish menjelang Phase 1 airdrop launch

---

## Catatan tentang dokumen ini

Dokumen ini adalah **dokumen pertama** dalam program i18n Sentrix. Goal: membuat informasi tentang Sentrix Chain dapat diakses dalam Bahasa Indonesia, bukan hanya Inggris.

**Roadmap translasi (urut prioritas):**

1. ✅ ROADMAP_INDONESIA.md (file ini — strategi & roadmap)
2. ⏳ Faucet user guide
3. ⏳ Validator setup guide
4. ⏳ Smart contract developer quickstart
5. ⏳ Tokenomics overview
6. ⏳ Governance & multisig
7. ⏳ Airdrop claim guide

**Kontribusi translasi:** translator dari komunitas Indonesia sangat dibutuhkan. PR dapat di-submit ke `sentrix-labs/sentrix` dengan path `docs/i18n/<filename>.md`. Style guide: pertahankan istilah teknis Inggris (smart contract, validator, mempool), translate konsep umum (governance → tata kelola, airdrop → airdrop [tetap], dll). Tidak terlalu kaku — Bahasa Indonesia conversational lebih baik daripada Bahasa Indonesia kaku translasi mesin.

---

## Cross-references

- [`docs/intro.md`](../intro.md) — Pengantar teknis chain (Bahasa Inggris)
- [`docs/tokenomics/OVERVIEW.md`](../tokenomics/OVERVIEW.md) — Detail suplai, halving, premine
- [`docs/tokenomics/AIRDROP_MECHANICS.md`](../tokenomics/AIRDROP_MECHANICS.md) — Mekanisme airdrop 5 fase
- [`docs/GOVERNANCE.md`](../GOVERNANCE.md) — Model governance multisig
- [`docs/security/AUDIT_SUMMARY.md`](../security/AUDIT_SUMMARY.md) — Status audit keamanan
