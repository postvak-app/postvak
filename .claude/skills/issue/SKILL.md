---
name: issue
description: Take a Postvak GitHub issue from open to merged. Reads the issue, asks the user what is unclear, checks whether main already changed anything related, updates or closes the issue when that is warranted, implements it, opens a PR, fixes CI and review findings, and enables auto-merge. Use when the user says "/issue 123", "pick up issue #123", "work on the next issue", "pick the best issue" or similar.
argument-hint: "[issue number or URL; empty = pick the clearest, most valuable one]"
---

# Work an issue end to end

Arguments: `$ARGUMENTS` — an issue number, an issue URL, or nothing.

Repository: `postvak-app/postvak`. Use the GitHub MCP tools (`mcp__github__*`, load them with ToolSearch) where they exist; fall back to `gh` when they don't. `AGENTS.md` is the law for everything below; this skill only says in what order to do things and when to stop and ask.

The user wants to be asked when something is unclear, and wants pushback when the issue's idea is flawed. Asking is cheap; building the wrong thing is not. But never ask what the code, the spec or a sensible default can answer.

## 1. Pick the issue

- With an argument: read that issue, its comments, labels, linked PRs and sub-issues (`issue_read`).
- Without one: choose the issue yourself, as below, and carry on with it.
- If the issue is a pull request, or is closed, say so and stop.
- If it carries `in progress`, another session has claimed it. Read the claim comment and check its branch (`git ls-remote origin <branch>`, then the last commit date). Say who claimed it and when, and ask the user whether to leave it or take it over; never take it over silently.

### Choosing without a number

List every open issue (`list_issues`, state open, only the fields you need). Leave out:

- issues with an open PR that addresses them (`search_pull_requests`), the `in progress` label, or an assignee other than the user;
- umbrella issues that only track sub-issues;
- issues waiting on something outside this repo (an upstream release, a person) or on another open issue ("do this after #…");
- issues whose body says "Decision needed" when no decision is recorded in a comment or a parent issue.

Rank the rest on two things:

- **Clear path:** the issue names the cause and the code, proposes a fix and a regression test, and the fix stays in one or two modules. Check the cause still holds on `origin/main` (read the code it points at) before counting it.
- **Gain:** a bug before an enhancement. Then how bad it is (data loss or a security gap, then something that stops a common action working, then wrong output, then cosmetic) and how often users hit it. If a parent issue gives an order, such as "bugs first", follow it.

Pick the issue that scores best on both together; a small issue with a clear fix beats a large one with a bigger but vaguer gain. Tell the user which one you picked, why, and the next two with one line each. Then go on to claim it and to step 2 without waiting; if the user names another, release the claim and switch.

### Claim it

Every session acts as the user's GitHub account, so an assignee can't tell sessions apart; the label and a comment carry the claim.

1. Add the `in progress` label. With `issue_write` (method `update`) the `labels` list replaces the issue's labels, so send its current labels plus `in progress`, never `in progress` alone. GitHub creates the label the first time it's applied; with `gh`, `gh issue edit <n> --add-label "in progress"`.
2. Comment: `Claimed by a Claude Code session on branch <branch>.`
3. Re-read the issue's comments. If another session's claim comment is older than yours (the two raced), that session has it: comment that you're backing off, leave the label on (it's theirs), and tell the user.

Release the claim whenever this session stops working on the issue without a PR that closes it: the user drops it, the issue is closed in step 2, or it's split into other issues. Remove the label (current labels minus `in progress`) and comment that the claim is released. Once the PR merges and closes the issue, remove the label too, so closed issues don't show up as in progress.

## 2. Check what main already did

Before forming a plan, find out whether the problem still exists:

1. `git fetch origin main` and work from `origin/main`.
2. Find the commits since the issue was opened that touch the same area: `git log --since=<issue created_at> origin/main -- <paths>` for the files and modules the issue points at (use the module map in `AGENTS.md` to find them), and `git log --grep` on the issue number and key words.
3. Search PRs, open and merged, that mention the issue or its subject (`search_pull_requests`).
4. Read the spec it touches in `docs/superpowers/specs/` and the docs in `docs/src/`.
5. For a bug, try to reproduce it on current main: write the failing test first if you can.

Then decide, and say which with the evidence (commit hashes, PR numbers, test output):

| Finding | Action |
|---|---|
| Fully fixed or built on main | Propose closing it: a comment naming the commit/PR, then close with `state_reason: completed`. Ask the user before closing. |
| No longer relevant (design changed, duplicate) | Propose closing as `not_planned` (or `duplicate`, naming the other issue). Ask first. |
| Partly done, or the issue's description is out of date | Propose a comment (or body edit, if the user wrote the issue) saying what main already does and what is left. Ask first, then post it. |
| Still open as written | Carry on. |

## 3. Clear up what's unclear

Read the issue as a spec and list what it leaves open: behaviour in edge cases, which surfaces it ships on (`AGENTS.md`: CLI, MCP and GUI together unless the spec says otherwise), which platforms (macOS and Linux; KDE Plasma on Wayland first), UI wording and placement, config or schema changes, migrations.

- Answer what you can from the code, the specs and `docs/src/`.
- Ask the rest in **one** round with `AskUserQuestion` (up to 4 questions, a recommended option first). Don't drip questions one by one.
- If the request has a flaw (it conflicts with the spec, a privacy rule, a platform rule, or there's a simpler way to get the same result), say so plainly and propose the alternative before writing code.
- If answering needs a decision big enough to change the issue, offer to record it as a comment on the issue.

Then give a short plan: what changes, in which modules, which tests, whether a spec in `docs/superpowers/specs/` is new or updated (new behaviour gets one, like the recent features), and the PR title. Start on it unless the user objects; for a large or risky plan, wait for a yes.

If the issue is too big for one reviewable PR, propose splitting it into stacked PRs (see "Stacked PRs" in `AGENTS.md`) or sub-issues, and ask.

## 4. Build it

- Branch: the session's designated branch if there is one, otherwise `git checkout -b <type>/<issue>-<slug> origin/main`. If it depends on an open PR, branch from that PR's branch and stack.
- Follow `AGENTS.md`: logic in a library module, IMAP through the daemon, tests for new logic, a regression test for a bug, no `unwrap`/`expect`/`panic!` outside tests, a reason for every new dependency, docs in `docs/src/`.
- Don't broaden the task. Something adjacent you notice goes to the user as a suggestion for a new issue, not into this PR.
- If something becomes unclear while building (the code contradicts the issue, a choice has visible consequences), stop and ask. Don't guess silently.
- Commit with Conventional Commits; the squash-merge title drives release-plz's changelog and version bump, so `feat`, `fix`, `docs`, etc. must be accurate and a breaking change gets `!`.

Before pushing, run the Definition of done from `AGENTS.md`:

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test            # or cargo nextest run
cargo machete
cargo deny check
```

Plus, when they apply: `POSTVAK_BLESS=1 cargo test` after CLI flag or rule type changes; `TZ=UTC UPDATE_SNAPSHOTS=1 cargo test -p postvak-gui snapshots` after GUI work, then look at every changed PNG yourself; `packaging/icons.sh` after icon changes. Commit what they regenerate. Note which platform you compiled on and which you ran on.

Re-read the full diff (`git diff origin/main...`) adversarially before pushing.

## 5. Open the PR

- Push with `git push -u origin <branch>`.
- Title: Conventional Commit, same style as `git log origin/main` (e.g. `feat(gui): …`, `fix(sync): …`).
- Body: fill in `.github/pull_request_template.md` (What and why, How it was tested, Checklist), with `Closes #<issue>` in "What and why". "How it was tested" says "compiled on" vs "ran on" honestly. Tick only what you actually ran.
- Subscribe to the PR's activity (`subscribe_pr_activity`) so CI and review events wake the session.

## 6. Drive it to green

On every CI result or review event, look at the whole PR on its current head:

- **CI red:** reproduce locally, find the root cause, fix, re-run the local checks, push. Never skip, disable or quarantine a test, never push an empty commit to re-trigger CI. A failure that is red on main too is not this PR's: say so on the PR instead.
- **Merge conflict:** merge `origin/main` into the branch (no rebase, no force-push), regenerate generated files with the repo's tooling, re-run the checks, push.
- **Review comments:** fix small asks and push; reply on the thread and resolve it. A larger ask or a design question goes to the user before you act.
- Keep going until CI is green on the head commit and no review thread is waiting on you.

## 7. Auto-merge

Once the PR is open and the local checks passed, enable auto-merge with **squash** (`enable_pr_auto_merge`, or `gh pr merge --auto --squash`). `main` has a merge queue with required checks (`lint`, `test (macos-latest)`, `test (ubuntu-latest)`): auto-merge puts the PR in the queue once they pass, and the queue runs CI again on top of the latest main before merging. That is the safety net.

- If auto-merge can't be enabled, don't bypass the queue: wait until CI is fully green on the head and no thread is open, then add it to the queue (a squash merge through the API or `gh pr merge --squash`), with the PR title as the commit title.
- If the queue rejects it (a conflict with what merged ahead of it, or CI red on the combined commit), that's step 6 again: fix, push, re-enable auto-merge.
- Never merge a red, conflicted or changes-requested PR, and never bypass branch protection.
- If the user says "don't merge" at any point, disable auto-merge and stop there.

After the merge: confirm the issue closed (via `Closes #…`), close it by hand with a comment if it didn't, remove the `in progress` label, unsubscribe from the PR, run `cargo clean` to free the worktree's `target/` (several GB), and give the user a short summary: the issue, the PR, what changed, what was compiled vs run where, and anything left over worth a new issue.
