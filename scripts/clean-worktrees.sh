#!/usr/bin/env bash
# Remove the git worktrees whose work is all on main, with their branches.
#
# Agents working beside each other get a worktree each (Claude Code puts
# them in .claude/worktrees/), and each builds its own target/ and
# target-web/: tens of GB that stay until the worktree goes. Kept:
#   * a worktree with uncommitted or untracked files;
#   * one with commits that aren't on main (compared by content, so commits
#     rebased onto main count as on it);
#   * one with anything changed in the last hour: an agent may still be
#     working there.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
main=$(pwd)
removed=0

consider() {
  local wt=$1 branch=$2 rev
  [ -d "$wt" ] || return 0 # gone already: `git worktree prune` below
  if [ -n "$(git -C "$wt" status --porcelain)" ]; then
    echo "kept $wt: it has uncommitted changes"
    return
  fi
  rev=${branch:-$(git -C "$wt" rev-parse HEAD)}
  if git cherry main "$rev" | grep -q '^+'; then
    echo "kept $wt: it has commits that aren't on main"
    return
  fi
  if [ -n "$(find "$wt" -maxdepth 3 -mmin -60 -print -quit)" ]; then
    echo "kept $wt: changed in the last hour (an agent may be working there)"
    return
  fi
  git worktree remove --force "$wt"
  [ -z "$branch" ] || git branch -q -D "$branch"
  echo "removed $wt${branch:+ and its branch $branch}"
  removed=$((removed + 1))
}

wt='' branch=''
while IFS= read -r line; do
  case "$line" in
    "worktree "*) wt=${line#worktree } ;;
    "branch refs/heads/"*) branch=${line#branch refs/heads/} ;;
    "")
      if [ -n "$wt" ] && [ "$wt" != "$main" ]; then consider "$wt" "$branch"; fi
      wt='' branch=''
      ;;
  esac
done < <(git worktree list --porcelain; echo)
git worktree prune
echo "$removed worktree(s) removed"
