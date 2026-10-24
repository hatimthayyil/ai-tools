# Patch-stack tools for agents

Written by Claude Opus 5.5.

Which tool helps agents keep logical commits, judged against how we work: forks in git worktrees (`isolation: "worktree"`), merged into main by fast-forward or cherry-pick, no forge, plain git still in use, `ai` reading repos with gix.

## The failure

Agents leave a large uncommitted pile and then collapse it into one commit. Splitting the pile afterwards is the expensive step: `git add -p` is interactive. The fix is to commit each step as it passes checks, then fold later corrections into the right earlier commit without an interactive rebase. A tool removes the cost of the second half. The first half is an instruction, whatever the tool.

## Candidates

| | git-absorb | git-revise | StGit | git-stack | Jujutsu (jj) | GitButler `but` |
|---|---|---|---|---|---|---|
| Model | Folds staged hunks into the commit they belong to (hg absorb port) | In-memory rebase; folds staged changes into a named commit | Stack of patches on a branch | Stacked branches, restacking | Working copy is always a commit; workspaces | Virtual/stacked branches in one working directory |
| Non-interactive fix of an earlier commit | `git absorb --and-rebase` (verified) | `git revise <sha>` | `stg refresh --patch <name>` | `git commit --fixup` + `git stack --rebase` | `jj absorb`, `jj squash --into` | `but absorb`, `but amend` |
| Non-interactive split of a pile | No | No (`--cut` is interactive) | No | No | By path (`jj split <paths>`); hunks interactive | **By hunk ID**: `but commit --changes <ids>` |
| Fits git-worktree forks | Yes, no state | Yes, no state | Per-branch `stg init`; metadata ref `refs/stacks/<branch>` | Yes | Since 0.46.0 (2026-10-07): colocated workspaces create git worktrees; a `WorktreeCreate` hook could call `jj workspace add` | No: replaces worktrees with lanes in one directory; `but worktree` is experimental behind a flag |
| Plain git alongside | Fully | Fully | `git commit`/`rebase`/`merge` desync the stack; `stg repair` | Fully | git sees detached HEAD; `main` is a bookmark moved with `jj bookmark set` | Workspace mode checks out `gitbutler/workspace`; `pre-commit` hook blocks `git commit`; leave with `but teardown` |
| Signing | Fixups unsigned (libgit2); rebase step honours git config | Honours git config (unverified) | Has gpg support | Honours `commit.gpgSign`/`stack.gpgSign` | `signing.*`, `git.sign-on-push` | `gitbutler.signCommits` overrides `commit.gpgsign` |
| nixpkgs | 0.9.0 (current) | 0.8.0 | 2.6.1 (upstream 2.7.0) | 0.10.20 (current) | 0.46.0 (current) | 0.19.9 desktop only; nixpkgs notes `but` is not bundled |
| Maintenance | Small, mature; last release 2026-02 | Quiet (unverified) | One main maintainer (116 of last year's commits) | One main maintainer (Ed Page) | Large, active | Company, very active; FSL-1.1-MIT (non-compete for two years) |
| Model familiarity | High risk only in flags; one command | Low | Low | Low | Moderate; models default to git habits | Low; ships a skill (`but agent setup`) |

Signing is not a constraint here: ai-tools has `commit.gpgsign = false` and `tag.gpgsign = true`. Rewrites touch unpushed commits only, and tags are made by the user afterwards.

## How each fits the fork flow

- **git-absorb.** Drop-in. In a fork branch, the stack is the commits not reachable from any other local branch, so main's commits are never candidates. Merge forks keep using plain git. Verified in a scratch repo: a staged hunk landed in the commit that introduced the line, and `--and-rebase` finished without a prompt. That run had `GIT_EDITOR=true` in its environment, which Claude Code's shell provides. `--and-rebase` runs `git rebase --interactive --autosquash` and sets no sequence editor, so under a harness without it (pi, DSH, unverified) the rebase would open `$EDITOR` (`hx` here). Agents should run it as `GIT_SEQUENCE_EDITOR=: git absorb --and-rebase`.
  - It does not check the remote (its own TODO list). On main it could rewrite pushed commits. Use `--base origin/main` there.
  - Default stack limit is 10 commits; `absorb.maxStack` raises it.
- **git-revise.** Same niche as absorb, but the agent names the target commit. Absorb finds it.
- **StGit.** Adds per-branch state that every other git command can desync. Each fork would `stg init`; merges would need `stg commit` first. Friction with no gain over absorb.
- **git-stack.** Built for stacked pull requests. Mid-stack edits move the working tree (`git prev`, `git amend`). Little to gain for one-branch forks with no forge.
- **jj.** The strongest model: no uncommitted state, conflicts stored in commits, `jj absorb` built in, operation log undo. Colocated workspaces as git worktrees are three weeks old. Replacing Claude Code isolation needs `WorktreeCreate`/`WorktreeRemove` hooks; hook-made worktrees skip Claude Code's worktree lock, marker, sweep and `.worktreeinclude`, keep transcripts at the launch directory, and must be created outside the repo. Mixed use costs habits: git shows detached HEAD, and advancing main is `jj bookmark set main -r <change>`. The two articles report models reverting to git, stale-workspace errors after rewriting shared ancestors, and multi-minute hangs; minikin enforces jj-for-writes with a global CLAUDE.md and a `PreToolUse` hook.
- **GitButler.** Its agent features are the best fit for the failure itself: commit selected hunks by ID, absorb, oplog undo. But workspace mode owns HEAD and blocks `git commit`, which breaks the plain-git merge forks and the user's own git use. Lanes share one working directory, so parallel forks would share `target/`, test state and same-file edits, which the worktrees exist to avoid. The `but` CLI is not in nixpkgs. Single-branch mode (no workspace ref checked out) exists in the code, but whether `but commit --changes` works there inside a plain worktree is unverified.

## Recommendation

**git-absorb**, with a commit-as-you-go rule. It solves the half a tool can solve, adds no state, and leaves forks, merges, gix and the user's git untouched.

**Runner-up: jj**, once colocated workspaces have settled and if agents still fail to commit as they go. It removes the uncommitted pile instead of making it cheaper to avoid, at the cost of new habits and replacing Claude Code's worktree isolation with hooks.

GitButler is rejected for now: one shared working directory conflicts with isolated builds, workspace mode takes over HEAD, and the CLI is not packaged. Its hunk-ID commit is the feature to watch.

## Setup for git-absorb

1. **Package:** `git-absorb` in the config repo's home packages.
2. **Git config (global):**
   ```
   [absorb]
       maxStack = 50
       oneFixupPerCommit = true
   ```
3. **Agent instructions (core.md):**
   - Commit each logical step once it passes checks; never batch unrelated changes into one commit.
   - To fix an earlier commit in the branch: stage the fix, then `GIT_SEQUENCE_EDITOR=: git absorb --and-rebase`. On main, add `--base origin/main`.
   - Hunks absorb cannot place stay staged: commit them as their own step, or `git commit --fixup=<sha>` then `GIT_SEQUENCE_EDITOR=: git rebase -i --autosquash <base>`.
   - Never rewrite pushed commits.
4. **Fork briefs:** "one commit per logical step".
5. **Claude Code hooks:** none required. An optional `SubagentStop` hook could warn when a fork ends with uncommitted changes, which is the failure itself.

## Sources

- Local checkouts: `../git-absorb` (0.9.0, `src/lib.rs`, `src/stack.rs`, `Documentation/git-absorb.adoc`), `../stgit` (2.7.0, `src/cmd/repair.rs`, `refresh.rs`), `../git-stack` (0.10.20, `docs/reference.md`), `../gitbutler` (`crates/but/src/args`, `crates/but-core/src/settings.rs`, `ref_metadata.rs`, `LICENSE.md`).
- nixpkgs versions via `nix eval nixpkgs#<pkg>.version`; `pkgs/by-name/gi/gitbutler/package.nix`.
- jj CHANGELOG 0.46.0: https://github.com/jj-vcs/jj/blob/main/CHANGELOG.md
- https://minikin.me/blog/jj-workspaces-for-parallel-agents/
- https://geirsson.com/jj-workspaces
- https://docs.gitbutler.com/features/branch-management/stacked-branches
- https://docs.gitbutler.com/workspace-branch
- https://docs.gitbutler.com/ai-agents/overview
- https://dev.to/maverickdotdev/we-ditched-worktrees-for-claude-code-heres-what-we-use-instead-5b22
- Claude Code docs: `hooks.md` (WorktreeCreate), `worktrees.md` (non-git version control).
