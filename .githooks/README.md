# Git hooks

Tracked git hooks that enforce CorTeX's quality gates.

- **pre-commit** — `cargo fmt --all --check` (fast, no compile). Blocks unformatted commits.
- **pre-push** — `cargo fmt --all --check` + `cargo clippy --all-targets -- -D warnings`.

## Enable them (once per clone)

```bash
git config core.hooksPath .githooks
```

`core.hooksPath` is a local setting, so each clone opts in once. To format/lint manually:

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```
