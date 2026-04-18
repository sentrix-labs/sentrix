# Branch Protection Rules

## main branch

Configure at: GitHub → Settings → Branches → Add rule → Branch name pattern: `main`

### Rules to enable

- [x] Require a pull request before merging
  - [x] Require approvals: 1
- [x] Require status checks to pass before merging
  - Required checks:
    - `Test` (from ci.yml — cargo test + clippy)
    - `Build` (from ci.yml — cargo build --release)
- [x] Require branches to be up to date before merging
- [x] Do not allow bypassing the above settings

### Setup steps

1. Go to https://github.com/sentrix-labs/sentrix/settings/branches
2. Click "Add branch protection rule"
3. Branch name pattern: `main`
4. Enable the rules listed above
5. Click "Create" / "Save changes"

### Notes

- After enabling, direct pushes to main will be blocked
- All changes must go through a PR with passing CI
- The repo owner can still bypass if "Do not allow bypassing" is unchecked
