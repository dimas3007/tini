# Contributing to tini

Thanks for your interest! Bug reports, ideas and pull requests are all welcome.

## Before you start

- For anything bigger than a small fix, open an issue first so we can agree on the approach.
- tini's main promise is being light: about 6–8 MB of private memory and 0% CPU at idle. Changes should keep it that way: no polling loops (use Windows events and one-shot timers), no extra resident threads or processes for optional features, and animations as Composition keyframes that DWM runs rather than per-frame redraws. If a change affects memory or CPU, mention before/after numbers in the PR (Task Manager's Memory column is fine).
- The UI is monochrome (black, white, translucent white) with a single accent, amber `#FFB340`, reserved for things that need attention. UI text is English.

## Building and testing

See [Build from source](README.md#build-from-source). Before opening a PR:

```powershell
cargo build --release
cargo test --workspace
```

The code is not run through `rustfmt` (it uses longer lines than rustfmt's default), so please don't reformat files you aren't otherwise changing.

Pure logic lives in modules with unit tests (`parse.rs`, `state.rs`, `geometry.rs`, `command.rs`, `deck.rs`, `pomodoro.rs`, `agents.rs`, the bridge's `install.rs` and `claude.rs`, …). Add tests there when you change behavior. Window, input and Composition code can't be unit-tested; describe how you checked it by hand.

A debug build (`cargo build`) opens a console with an event log. `notch.exe --glance "text"`, `--command "text"`, `--toggle` and the other flags in the README drive a running instance without real input.

## Code style

- Match the surrounding code: short functions, terse comments that explain *why*, no dead code.
- Keep `notch-bridge` free of the `windows` crate: it runs on every Claude Code hook and has to start fast.
- Never log, store or send credentials. The only network requests tini makes are the opt-in Claude usage calls (`api.anthropic.com`, plus `platform.claude.com` for token refresh); new network features must be opt-in too.

## Pull requests

- One topic per PR, with a clear description of what changed and why.
- Make sure `cargo test` passes on Windows; CI runs it on `windows-latest`.

## Scope: free core and Pro

Everything in tini's public repository is free and stays free. A future paid tini Pro may add *new* features on top; features on the Pro roadmap are not merged into the open-source core, so before working on something large, ask in an issue or discussion whether it fits the core. Nothing that is in the core today will move behind a paywall.

## Sign your commits (DCO)

tini uses the [Developer Certificate of Origin](https://developercertificate.org) instead of a CLA. Add a `Signed-off-by` line to every commit to certify that you wrote the change or otherwise have the right to submit it under the project's license:

```powershell
git commit -s -m "Fix the glance width on 125% scaling"
```

The sign-off must use your real name (or the name you're known by) and an email you can be reached at; a GitHub noreply address is fine.

## License

By contributing, you agree that your contributions are dual licensed under the MIT and Apache-2.0 licenses, as described in the [README](README.md#license), without any additional terms or conditions. The names "tini" and "Nochi" are covered by [TRADEMARKS.md](TRADEMARKS.md).
