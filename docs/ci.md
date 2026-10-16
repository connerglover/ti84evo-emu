# GitHub Actions

**CI** is the entry point for pushes to `main`, `v*` tags, pull requests, and
manual runs. It has two platform check jobs and one dependent Windows package
job. The package implementation lives in `release.yml`, a reusable workflow;
it no longer starts a separate, competing run on every push.

- **Linux:** formatting, USB adapter tests, Arm program image reproducibility,
  workspace Clippy/tests, and 10,000-case differential tests against Unicorn.
- **Windows:** USB adapter tests and workspace Clippy/tests with GUI snapshots
  enabled, including the USB snapshots in `connect.rs`.
- **Windows package:** runs after both check jobs pass on main/tag/manual runs.
  Builds GUI and CLI executables, uploads a ZIP for 14 days, and publishes a
  GitHub Release only when running on a tag. Pull requests do not package releases.

Linux and Windows failures are independent: neither cancels the other. A newer
run on the same branch/PR replaces an obsolete run; tag runs are not cancelled.
Branch pushes outside `main` use their pull-request run instead of duplicating it.
Hosted OS families, Rust, Arm GCC, Node, and action revisions are pinned. Rust
build caches are shared between steps in each job and reused across runs.

Failed Windows jobs upload snapshot evidence as `gui-snapshot-diffs`. Snapshots
are compared in CI, never regenerated there. Update expected images locally only
after inspecting an intentional UI change.

Arm `.bin` files are data, so the build script normalizes their mode to `0644`;
byte comparisons and untracked-output detection remain enabled. The toast layout
regression checks growth beyond the preceding short message rather than assuming
an exact label width from the runner's temporary-directory path.
