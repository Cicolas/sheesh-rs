---
name: ship-release
description: Review all open PRs targeting the current release branch, interactively ask whether each should be included, then create a PR from the release branch to main. Usage: /ship-release
---

Review open PRs targeting the current release branch, confirm which to include, and open a release PR to main.

## Steps

Follow these steps in order without skipping any.

### 1. Determine the release branch

Read the version from `Cargo.toml`:
```
grep '^version' Cargo.toml | head -1 | sed 's/.*= *"\(.*\)"/\1/'
```
The release branch is `release/v<version>` (e.g. `release/v0.1.0`).

### 2. List open PRs targeting the release branch

```
gh pr list --base <release-branch> --state open --json number,title,headRefName,author,url,body
```

If there are no open PRs, skip to step 4.

### 3. Ask the user about each open PR

For each PR found, display:
- PR number, title, author, branch name, URL

Then check the PR body for a "## Tests" section containing a markdown checklist. If any checklist items are unchecked (lines starting with `- [ ]`), warn the user:
> "⚠️  PR #<N> has unchecked test cases in the Tests section. Please verify them before including."
> List the unchecked items.

Then ask the user:
> "Include PR #<N> — <title> (<branch>)? [y/n]"

Wait for the user's answer before moving to the next PR. Collect the list of PRs the user said **yes** to.

For PRs the user said **yes** to, ensure they are merged into the release branch:
- If the PR is already merged, skip.
- If not yet merged, merge it:
  ```
  gh pr merge <number> --merge --auto
  ```
  Wait and confirm merge completed before continuing.

### 4. Check whether a release PR already exists

```
gh pr list --head <release-branch> --base main --state open --json number,url
```

If one already exists, print its URL and skip to step 6.

### 5. Create the release PR

```
gh pr create \
  --base main \
  --head <release-branch> \
  --title "Release v<version>" \
  --body "$(cat <<'EOF'
## Release v<version>

### Included changes
<bullet list of merged PR titles and numbers that were included>

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

### 6. Report

Print a short summary:
- Release branch
- PRs included (title + number)
- PRs skipped (title + number)
- Release PR URL
