# Benchmark Results

This branch is auto-populated by `.github/workflows/benchmark.yml` via
[`benchmark-action/github-action-benchmark`](https://github.com/benchmark-action/github-action-benchmark).

Don't push to this branch directly — it's the persistence layer for
criterion bench baselines. Every push to `main` that touches the
consensus crates updates `bench/data.js` with the latest numbers; PRs
read from here to detect regressions.

Visualisation: <https://sentrix-labs.github.io/sentrix/bench/> (once
GitHub Pages is enabled on this branch).
