window.BENCHMARK_DATA = {
  "lastUpdate": 1778666044157,
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
      }
    ]
  }
}