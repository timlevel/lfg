# Working in lfg

Read README.md and CLAUDE.md before changing the application. Keep each issue to
one reviewable PR and use a feature worktree; the primary checkout stays clean
on `main`.

## Verify

There is no repository verify script or CI workflow. Run the documented Rust
checks locally before pushing and include the results in the PR. Linux builds
need `pkg-config` and D-Bus development headers (for example, `libdbus-1-dev`):

```bash
cargo check --locked
cargo clippy --locked
cargo test --locked
```

These commands build/check the application without starting it. Do not use
`cargo run`, webhook examples, or mutation endpoints to verify documentation.

## Deploy

The existing installation uses `~/.local/bin/lfg` under the user systemd unit
`lfg.service`, with `--no-ble --bind 127.0.0.1 --port 5555`. Preserve those
arguments; enabling BLE or changing network exposure needs separate review.
The infra manifest entry is `unmanaged`: `bin/infra deploy lfg` and
`bin/infra apply lfg` refuse it. There is no deploy script in this repository.

For documentation-only changes, after the PR is approved and merged,
fast-forward the clean primary checkout to `origin/main` and run the checks
above. Do not install a binary, alter the unit, restart the service, or change
BLE, credentials, or state as part of that work.

For a separately authorized application update:

1. Fast-forward the clean primary checkout to the merged `origin/main` and run
   the checks above. Build with `cargo build --release --locked`.
2. Inspect the existing unit locally to confirm its working directory and
   optional environment-file configuration. Keep those settings and the
   existing service arguments. Do not copy local configuration into git or PRs.
3. Replace only `~/.local/bin/lfg` with the built `target/release/lfg`, retaining
   the previous binary outside git for rollback. Do not run a second server.
4. Restart the existing unit with `systemctl --user restart lfg.service`, then
   perform the read-only health checks below. A restart interrupts requests and
   resets in-memory agent state. If health fails, restore the previous binary
   and restart the same unit; never reset the database to repair a deployment.

The application opens `lfg.db` relative to its working directory. The existing
unit uses `%S/lfg` (normally `~/.local/state/lfg`), separate from the checkout.
Preserve that directory, the database, and any SQLite sidecar files. Never run
`POST /reset`, delete state, or change the working directory during deployment.

Authentication is optional. Preserve the existing external environment file and
`LFG_TOKEN` configuration, including the absence of a token. Keep tokens outside
git and out of commands, logs, screenshots, and PR text. Source supports
`LFG_TOKEN` as an alternative to `--token`; do not rotate or introduce credentials
as part of deployment documentation work.

Read-only health checks:

```bash
systemctl --user is-active lfg.service
curl -fsS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:5555/status
```

Expect `active` and HTTP `200`. If authentication is enabled, use the existing
Bearer token through a secure local mechanism without printing it; an
unauthenticated `401` is not a successful health check. Do not post the status
response body, which can contain agent activity. Health checks must not send
webhooks, change themes, export debug files, or reset state.
