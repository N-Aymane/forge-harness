# Validation status

The source implementation includes the CLI, terminal UI, streaming provider adapters, repository tools, approval flow, persistent sessions, accounting ledger, budgets, context compaction, and cross-platform CI packaging.

## Checks completed

- `cargo fmt --all -- --check` passes.
- `cargo metadata --locked --offline --no-deps --format-version 1` passes.
- Cargo dependency resolution/fetch completed; `Cargo.lock` is included.
- `Cargo.toml`, `Cargo.lock`, and both example TOML files parse successfully.
- In a temporary Vercel Linux sandbox: Clippy with warnings denied passes on Rust 1.88.0 (MSRV) and stable 1.99.1.
- `cargo test --locked --all-targets`: **31 passed, 0 failed, 2 ignored**. The ignored subprocess helper is exercised by its parent test; the paid API smoke test remains opt-in.
- A real POSIX pseudo-terminal test passes: prompt submission, keyboard approval, file writing, agent follow-up, resizing, and restoration of terminal settings on exit.
- Optimized Linux x86_64 release build, plus executable version/doctor smoke checks, pass.
- Windows x86_64 GNU release cross-compilation from Linux passes.
- Windows-target Clippy with warnings denied also passes on Rust 1.88.0.
- Native Windows, Linux, and macOS formatting, Clippy, test suites, and optimized release builds pass in [publication CI](https://github.com/N-Aymane/forge-harness/actions/runs/41736340060). Platform build archives and Linux test captures are attached to that run.

## Local host restriction and remaining limits

Debug, minimum-toolchain, and release-mode compilation were attempted. Windows Code Integrity blocked generated build-script executables and a dependency's procedural-macro DLL. The system reported error 4551, and Code Integrity events 3033/3077 confirmed an Enterprise signing-policy rejection. No Windows security settings were changed.

Linux compilation and tests were subsequently verified using the sandbox, followed by native Windows/Linux/macOS CI. The native CI tests exercise CLI commands and approved shell execution; real interactive PTY approval/resize/cleanup was tested on Linux. Manual interactive terminal checks on Windows/macOS and paid-provider integration remain unverified. No real-provider API requests were made. Clippy found keyboard-handler spacing issues and two nested conditionals; these were corrected locally and rechecked before the successful builds.

The GitHub Actions workflow in `.github/workflows/ci.yml` runs formatting, Clippy, tests, and release builds on Windows, Linux, and macOS, plus a Rust 1.88 compatibility check. It uploads release archives after successful checks. Linux also generates real PTY screenshots and their terminal evidence. The workflow can be dispatched manually; documentation-only changes do not trigger unnecessary builds.

The opt-in paid smoke test is ignored by default and requires explicit model/config environment variables, configured pricing, and a $0.01 local reservation budget. All other tests use deterministic mocks, local HTTP fixtures, or temporary directories.

## Reproduce verification

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
python3 tests/tui_smoke.py target/debug/forge # POSIX only
cargo build --locked --release
```

Configure a real model using `config.example.toml`. Run `forge doctor` to locate the user configuration and database paths. Follow `README.md` for onboarding, key storage, budgets, and command usage.

## Release archives and screenshots

The CI workflow generates platform release archives and a `forge-test-screenshots` artifact containing actual PTY captures and test output. The public setup guide explains how to build locally and obtain these artifacts. No package-manager or crates.io release has been published.

Five verified captures from source commit `f71d346a41c3fb780bf09376aa108227ac325c1c` are committed in `docs/images/` and embedded in the setup guide. Their `.ansi`, `.txt`, and `.json` evidence and artifact provenance are included. Images use a deterministic local provider, not a paid model.
