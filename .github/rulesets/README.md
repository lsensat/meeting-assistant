# Rulesets

Ready to apply, not applied. GitHub refuses rulesets on a private repository on
the free plan — `403: Upgrade to GitHub Pro or make this repository public` — so
these sit here until the repository is public, and then go on in one command
each:

```bash
gh api repos/lsensat/meeting-assistant/rulesets --input .github/rulesets/main-branch.json
gh api repos/lsensat/meeting-assistant/rulesets --input .github/rulesets/release-tags.json
```

Check them afterwards with `gh api repos/lsensat/meeting-assistant/rulesets`, and
remove one with `gh api -X DELETE repos/lsensat/meeting-assistant/rulesets/<id>`.

## `main-branch.json`

Requires a pull request, blocks force pushes, blocks deletion.

**`required_approving_review_count` is deliberately 0.** GitHub does not let you
approve your own pull request, so on a single-maintainer repository any higher
number locks you out of your own `main` with no way to merge. The rule still
earns its place: it makes an accidental `git push` to `main` impossible, and it
guarantees every change arrives as a reviewable diff.

`bypass_actors` is empty on purpose. Adding yourself would make the rule
advisory, which is the same as not having it. If something genuinely needs to go
around it, deleting the ruleset for a minute is more honest — and leaves a trace.

## `release-tags.json`

Blocks deleting, moving or force-updating any `v*` tag. Creation is left alone,
since cutting a release is exactly what these tags are for.

The point is that draft releases hang off these tags. A moved tag silently
detaches a published release from the code it was built from, and nothing in the
release page says so.

## Not included: required status checks

Deliberately absent, because adding it today would be a trap. `release.yml` runs
on `push` to `v*` tags, `workflow_dispatch` and a weekly schedule — **there is no
`pull_request` trigger**, so CI has never run on a pull request. A ruleset
requiring checks to pass would leave every PR waiting on a check that never
starts, against a `main` that cannot be pushed to directly.

To add it, in this order:

1. Add a `pull_request` trigger to `release.yml`. Note this costs Actions minutes
   on a private repository — free on a public one, which is the same condition
   that unlocks rulesets at all.
2. Merge one PR so the check names appear in the API.
3. Add a `required_status_checks` rule naming `Test` and `Lint`.
