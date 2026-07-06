---
name: release
description: Draft release notes from the commits since the previous release, bump the version, and create a GitHub Release. Use when the user says "release it" / "publish a new version" or invokes /release.
---

# Release procedure

Publish a new version of lapse to GitHub Releases. The Release is created with
`gh release create`, and the tag push triggers `.github/workflows/release.yml`,
which attaches binaries for 5 targets.

## Steps

1. **Preflight checks** (if any of them fails, stop and report to the user)
   - `git status` — the working tree is clean
   - The current branch is `main` and in sync with `origin/main` (`git fetch && git status`)
   - CI for HEAD succeeded: the latest run in `gh run list --branch main --limit 1` is success

2. **Collect the changed commits**
   - Previous tag: `git describe --tags --abbrev=0`
   - Change list: `git log <previous tag>..HEAD --oneline`
   - If there are zero changes, stop and report to the user

3. **Decide the version number** (pre-1.0 policy)
   - Behavior changes, new features, or CLI interface changes → bump minor (0.x.0)
   - Bug fixes, documentation, or internal refactoring only → bump patch (0.x.y)
   - When in doubt, ask the user

4. **Draft the release notes**
   - Do not paste commit messages verbatim; rewrite them **from the tool user's point of view**
   - Write in English, categorized (omit sections with no entries): `## New Features` `## Changes` `## Fixes` `## Internal`
   - For behavior changes, always make "it used to work like this → now it works like this" clear
   - Internal refactors (type introductions, CI additions, and other changes invisible to users) get one concise line each under "Internal"

5. **Commit the version bump**
   - Update `version` in `Cargo.toml` and run `cargo build` to update `Cargo.lock` too
   - Commit with the message `vX.Y.Z` and push

6. **User confirmation** (required — never skip)
   - Present the version number and the full release notes, and get approval to publish
   - If corrections are requested, fix the notes and confirm again

7. **Create and verify the release**
   - `gh release create vX.Y.Z --title "vX.Y.Z" --notes "<notes>"` (this also creates the tag)
   - The tag push starts the Release workflow; wait for completion with `gh run watch`
   - Confirm with `gh release view vX.Y.Z --json assets` that all 5 binaries
     (Linux x2 / macOS x2 / Windows x1) are attached, and report the release URL to the user

## Notes

- The workflow's `softprops/action-gh-release` does not overwrite the body of an
  existing release, so hand-written notes are preserved
- To fix the notes after releasing: `gh release edit vX.Y.Z --notes "<revised>"`
