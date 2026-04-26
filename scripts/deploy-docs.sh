#!/usr/bin/env bash
# deploy-docs.sh — rebuild + deploy docs.sentrixchain.com from latest main
#
# Run after merging changes to docs-site/ on main. Pulls latest, builds
# Docusaurus, rsyncs to /var/www/docs-sentrixchain/ (where Caddy file-serves).
#
# Idempotent — safe to re-run. ~30s on warm caches.
set -euo pipefail

REPO=/home/sentriscloud/sentrix
WEB_ROOT=/var/www/docs-sentrixchain
DOCS_DIR="${REPO}/docs-site"

echo "==> deploy-docs.sh — $(date -Iseconds)"

# 1. Sync repo to latest main
echo "==> git pull origin main"
cd "$REPO"
git fetch origin main
local_head=$(git rev-parse HEAD)
remote_head=$(git rev-parse origin/main)
if [ "$local_head" != "$remote_head" ]; then
    echo "    local=$local_head remote=$remote_head — fast-forwarding"
    git checkout main
    git pull --ff-only origin main
else
    echo "    already at main HEAD ($local_head)"
fi

# 2. npm install if package-lock changed
echo "==> npm ci (idempotent)"
cd "$DOCS_DIR"
npm ci --silent

# 3. Production build
echo "==> npm run build"
npm run build 2>&1 | tail -10

# 4. Atomic rsync to web root (delete stale files)
echo "==> rsync to ${WEB_ROOT}"
sudo rsync -a --delete "${DOCS_DIR}/build/" "${WEB_ROOT}/"
sudo chown -R sentriscloud:sentriscloud "${WEB_ROOT}/"

# 5. Verify Caddy is serving correctly (no reload needed — file_server picks up changes)
echo "==> verify https://docs.sentrixchain.com"
sleep 2
http_code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 5 https://docs.sentrixchain.com)
if [ "$http_code" = "200" ]; then
    echo "    HTTP $http_code ✓"
    echo "==> deploy complete"
else
    echo "    HTTP $http_code — UNEXPECTED, investigate"
    exit 1
fi
