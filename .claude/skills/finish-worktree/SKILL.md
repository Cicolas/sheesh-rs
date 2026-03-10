---
name: finish-worktree
description: Commit all changes in the current worktree and open a PR targeting the shared release branch. Usage: /finish-worktree [release-branch] — defaults to "release/next".
argument-hint: "[release-branch]"
---

Finish the current worktree by committing all changes and creating a PR to the shared release branch.

## Steps

Follow these steps in order without skipping any.

### 1. Determine context

Run these in parallel:
- `git status` — list modified/untracked files
- `git diff` — see all changes
- `git log --oneline -5` — recent commits for style reference
- `git branch --show-current` — current branch name

### 2. Determine the release branch

If the user provided an argument (e.g. `/finish-worktree release/v2`), use that as the release branch name.
Otherwise use `release/next` as the default.

### 3. Ensure the release branch exists on the remote

Run: `git ls-remote --heads origin <release-branch>`

If it does NOT exist, create and push it from main:
```
git fetch origin main
git push origin origin/main:refs/heads/<release-branch>
```

### 4. Pre-flight checks

Run the following checks and fix any issues before committing. Do NOT proceed to commit if these fail.

```
cargo clippy -- -D warnings
cargo build
```

If clippy reports warnings/errors, fix them first, then re-run until clean.

### 5. Stage and commit all changes

Stage all modified tracked files (do NOT use `git add -A` to avoid accidentally committing secrets or build artifacts — use `git add -u` for tracked files and explicitly add any new source files under `src/`):
```
git add -u
git add src/  2>/dev/null || true
```

Draft a commit message by analysing the diff. Follow the existing commit style from the log.
Commit:
```
git commit -m "$(cat <<'EOF'
<your message here>

Co-Authored-By: Claude Sonnet 4.6 <noreply@anthropic.com>
EOF
)"
```

If there is nothing to commit (working tree clean), skip to step 5.

### 6. Push the current branch to remote

```
git push -u origin HEAD
```

### 7. Create the PR

Use `gh pr create` targeting the release branch:
```
gh pr create \
  --base <release-branch> \
  --title "<short title>" \
  --body "$(cat <<'EOF'
## Summary
- <bullet points summarising what changed>

## Notes
Part of a batch of worktree changes targeting `<release-branch>`.

## Tests
Task list containing tests cases for the new feature

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

If a PR already exists for this branch (`gh pr list --head <branch>`), skip creation and just print the existing PR URL.

### 8. Report

Print a short summary:
- Commit hash (or "nothing to commit")
- PR URL
- Release branch targeted