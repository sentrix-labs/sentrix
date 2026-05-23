window.BENCHMARK_DATA = {
  "lastUpdate": 1779529674617,
  "repoUrl": "https://github.com/sentrix-labs/sentrix",
  "entries": {
    "sentrix-trie benches": [
      {
        "commit": {
          "author": {
            "email": "49699333+dependabot[bot]@users.noreply.github.com",
            "name": "dependabot[bot]",
            "username": "dependabot[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "ab8ce3413e71f9d4cc4d910a2a535222a6d6f90f",
          "message": "chore(deps): bump criterion from 0.5.1 to 0.8.2 (#631)\n\nBumps [criterion](https://github.com/criterion-rs/criterion.rs) from 0.5.1 to 0.8.2.\n- [Release notes](https://github.com/criterion-rs/criterion.rs/releases)\n- [Changelog](https://github.com/criterion-rs/criterion.rs/blob/master/CHANGELOG.md)\n- [Commits](https://github.com/criterion-rs/criterion.rs/compare/0.5.1...criterion-v0.8.2)\n\n---\nupdated-dependencies:\n- dependency-name: criterion\n  dependency-version: 0.8.2\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>",
          "timestamp": "2026-05-13T16:51:49+07:00",
          "tree_id": "6d9a725b9ffa6b0ceec7b0e8da24a6b681fe41e8",
          "url": "https://github.com/sentrix-labs/sentrix/commit/ab8ce3413e71f9d4cc4d910a2a535222a6d6f90f"
        },
        "date": 1778666043239,
        "tool": "cargo",
        "benches": [
          {
            "name": "insert_single",
            "value": 165367,
            "range": "± 7417",
            "unit": "ns/iter"
          },
          {
            "name": "insert_batch_100",
            "value": 487993,
            "range": "± 11758",
            "unit": "ns/iter"
          },
          {
            "name": "commit_after_100_inserts",
            "value": 1453833,
            "range": "± 1587914",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "119509589+satyakwok@users.noreply.github.com",
            "name": "satyakwok",
            "username": "satyakwok"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "1c6959d8863825e4e2f0810d202f985a6ef7bac1",
          "message": "Fix/testnet fork defaults v2.2.11 (#687)\n\n* fix(core): bake deterministic testnet fork defaults\n\nBake mature testnet fork heights for chain id 7120\n\nPreserve mainnet/unknown-chain defaults as the v2.2.11 safe defaults\n\nKeep risky forks disabled pending replay/determinism audits\n\nDo not change genesis, chain id, validator keys, balances, or deployment config\n\nTestnet-only safety improvement to avoid ENV-only fork behavior\n\n* fix(core): honor testnet trie integrity skip env\n\nRestore deployed runtime compatibility for SENTRIX_SKIP_TRIE_INTEGRITY=1\n\nKeep the skip strict to the explicit value 1\n\nRequired for testnet nodes whose historical trie tables contain known orphan references",
          "timestamp": "2026-05-17T05:20:49+07:00",
          "tree_id": "07d3bf8df1c88b756ee5c50e5a888e583fc9e275",
          "url": "https://github.com/sentrix-labs/sentrix/commit/1c6959d8863825e4e2f0810d202f985a6ef7bac1"
        },
        "date": 1778970168732,
        "tool": "cargo",
        "benches": [
          {
            "name": "insert_single",
            "value": 171618,
            "range": "± 10385",
            "unit": "ns/iter"
          },
          {
            "name": "insert_batch_100",
            "value": 545418,
            "range": "± 26990",
            "unit": "ns/iter"
          },
          {
            "name": "commit_after_100_inserts",
            "value": 1369676,
            "range": "± 1965154",
            "unit": "ns/iter"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "49699333+dependabot[bot]@users.noreply.github.com",
            "name": "dependabot[bot]",
            "username": "dependabot[bot]"
          },
          "committer": {
            "email": "noreply@github.com",
            "name": "GitHub",
            "username": "web-flow"
          },
          "distinct": true,
          "id": "c6ae2619a315fa5d84c10947f72ec83ed0faca3f",
          "message": "chore(deps): bump alloy-primitives from 1.5.7 to 1.6.0 (#698)\n\nBumps [alloy-primitives](https://github.com/alloy-rs/core) from 1.5.7 to 1.6.0.\n- [Release notes](https://github.com/alloy-rs/core/releases)\n- [Changelog](https://github.com/alloy-rs/core/blob/main/CHANGELOG.md)\n- [Commits](https://github.com/alloy-rs/core/compare/v1.5.7...v1.6.0)\n\n---\nupdated-dependencies:\n- dependency-name: alloy-primitives\n  dependency-version: 1.6.0\n  dependency-type: direct:production\n  update-type: version-update:semver-minor\n...\n\nSigned-off-by: dependabot[bot] <support@github.com>\nCo-authored-by: dependabot[bot] <49699333+dependabot[bot]@users.noreply.github.com>",
          "timestamp": "2026-05-23T16:45:52+07:00",
          "tree_id": "24a1f364a9dd1ba2ca0b1774beafee8eca8f74ee",
          "url": "https://github.com/sentrix-labs/sentrix/commit/c6ae2619a315fa5d84c10947f72ec83ed0faca3f"
        },
        "date": 1779529674214,
        "tool": "cargo",
        "benches": [
          {
            "name": "insert_single",
            "value": 83470,
            "range": "± 10721",
            "unit": "ns/iter"
          },
          {
            "name": "insert_batch_100",
            "value": 446055,
            "range": "± 27929",
            "unit": "ns/iter"
          },
          {
            "name": "commit_after_100_inserts",
            "value": 1290934,
            "range": "± 46775",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}