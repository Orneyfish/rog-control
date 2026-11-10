# Review rules

Read by the super-agents reviewer and by `/code-review`. Keep it short and specific to this project; delete what does not apply.

## What counts as a failure
- Behaviour differs from `specs/<feature>/RD.md` or from an eval in `specs/<feature>/evals.md`.
- Data loss, wrong data shown, or one user able to see or change another user's data.
- A crash, unhandled error, or silent failure on a path a user can actually reach.
- A missing or bypassable auth/permission check; a secret in code, logs or a response.
- UI that does not match `specs/<feature>/mockup.html` at desktop or mobile width.
- TODO: add this project's own failures (money, PII, migrations, external APIs, ...).

## Severity
- **blocker**: a user loses data/money/access, sees someone else's data, or the feature does not work.
- **major**: a reachable path breaks or misbehaves, but nothing is lost and a retry works.
- **minor**: cosmetic, wording, or an unlikely edge.
- Done requires: no open blocker or major (high-risk features: no open minor on auth/data paths either).

## Ignore
- Formatting, naming taste, and anything a linter already enforces.
- Theoretical problems with no reachable path.
- Code the change did not touch, unless the change made it reachable.

## Report format
One line per finding: `F<n> [severity] [class] file:line - what is wrong - how to reproduce`.
`class` names the kind of mistake, not the instance (e.g. `missing-auth-check`, `unhandled-empty-state`, `off-by-one`, `n-plus-one-query`), so it can be fixed everywhere it occurs.
Only report what you can show with evidence. Nothing found: write `no findings`.

## How to test this project
- TODO: run command, test command, URL and port, how to start with a temporary database / data dir, login for a test account.

## Rules learned over time
<!-- One line each. Add when a review missed a bug that surfaced later, or flagged the same non-problem more than once. -->
- Any value written into a unit or desktop file, or used as an install target (PREFIX, HOME, XDG_*), is validated up front, before any build or write: absolute, and no whitespace, quote, backslash, `%`, `$` or backtick (class `unvalidated-path-written-into-unit`).
- A test's skip condition comes from the machine, never from the code under test; every hardware write that can fail has a failure-toast test; a loop over a code-defined collection asserts it is non-empty; tests never claim fixed machine-global names (app ids, bus names, /tmp paths) (classes `vacuous-pass-on-missing-fixture`, `untested-failure-toast`, `skip-condition-derived-from-code-under-test`, `test-global-name-collision`).
