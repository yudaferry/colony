# Development

## Toolchain

The project pins **Rust 1.99.1** in `rust-toolchain.toml` (with rustfmt and clippy). The Nix flake builds with the same file through `rust-overlay`.

```bash
nix develop            # shell with the pinned toolchain
cargo test             # all tests
cargo clippy --all-targets
cargo fmt --check
nix build              # result/bin/colony
```

Without Nix, install Rust 1.99.1 (for example with rustup, which reads `rust-toolchain.toml`) and use cargo directly. `install-rust.sh` runs the whole test suite through the flake with limited CPU use (2 cores, 1 job), which is handy on a small machine; it writes its output to `test.log`, which is git-ignored.

The TLS stack pulls in `aws-lc-sys`, which compiles C code, so builds need `cmake`, `perl` and a C compiler (the flake adds the first two). The first build compiles many crates and can take a while.

## Tests

Tests live next to the code in `#[cfg(test)]` modules; there is no network and no real multiplexer involved. The seams are:

- **`Runner`** (`src/backend/runner.rs`): `Herdr` and `Tmux` run external commands through it. `FakeRunner` records every argument list and returns queued output, so tests assert the exact `herdr` / `tmux` calls (for example the layout resizes in [layout](layout.md)).
- **`FakeBackend` / `FakeBackends`** (`src/backend/fake.rs`, `mod.rs`): in-memory backends for command-level tests, such as one call reaching both herdr and tmux.
- **`Clock` and `Env`** (`src/ctx.rs`): `FixedClock` and `MapEnv` make time and backend detection deterministic.
- **`TelegramApi`** (`src/telegram/api.rs`): the bot and `reply` take a trait object; tests use a fake that records sent messages and returns canned updates.
- **State**: tests use a `tempfile` directory as `~/.colony`, including a concurrent-update test for the lock.

`tests/fixtures/herdr/` holds sample herdr output (`pane_list.json`, `agent_list.json`, `agent_read.txt`, `pane_split.json`) and `schema.json` from `herdr api schema`. They contain only neutral example paths and names; keep it that way when you refresh them from a real session.

## CI and releases

`.github/workflows/release.yml` runs on every push to `master` and on manual dispatch:

1. Install musl tools, cmake and perl; set up the toolchain from `rust-toolchain.toml` and the `x86_64-unknown-linux-musl` target.
2. `cargo test --locked`.
3. `cargo build --release --locked --target x86_64-unknown-linux-musl`, then check that the binary is static and runs.
4. Read the version from `Cargo.toml` and check whether the tag `v<version>` exists.
5. If the tag is **new**: pack `colony-<version>-x86_64-linux.tar.gz` plus a `.sha256` file, take the notes from the matching section of `RELEASE_NOTES.md`, and create the GitHub release and tag. If the tag exists, the run only tests and builds.

### Cutting a release

1. Bump `version` in `Cargo.toml` (and the `version` in `flake.nix`; run a build so `Cargo.lock` follows).
2. Add a `## colony <version> ...` section at the top of `RELEASE_NOTES.md`. The workflow copies the lines under the heading that starts with `## colony <version>` up to the next `## ` heading, and fails if the section is empty or missing.
3. Merge to `master`. The workflow creates the tag and the release.
