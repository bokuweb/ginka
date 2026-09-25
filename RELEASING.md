# Releasing

Ginka has not been released. This is the process the first release will
follow, written down before it happens (roadmap M6); the parts that do not
exist yet are marked as such.

## Versions

- One version for the workspace, in the root `Cargo.toml` and each crate's
  `Cargo.toml`, currently `0.0.0`.
- The wire protocol has its own number, `PROTOCOL_VERSION`, which moves with
  every change to the requests; a newer client replaces an older daemon, and
  never the other way round.

## Steps

1. Update `CHANGELOG.md`: move *Unreleased* under the new version and date.
2. Set the version in every `Cargo.toml`, run `cargo build` so `Cargo.lock`
   follows, and commit both with the changelog.
3. Check the tree is green: `cargo fmt --check`, `cargo clippy` with and
   without `--features github`, `cargo test --workspace`.
4. Tag the commit `vX.Y.Z` and push the tag.
5. Build the artifacts *(not automated yet)*:
   - macOS: `scripts/package-macos --universal` builds `Ginka.app` — the
     window as its executable, `ginka` and `ginka-daemon` beside it (N16) —
     and `Ginka-<version>.dmg`. Set `GINKA_SIGN_IDENTITY` to a Developer ID
     certificate to sign with the hardened runtime (ad hoc otherwise) and
     `GINKA_NOTARY_PROFILE` to a `notarytool` keychain profile to notarize
     and staple. CI checks the bundle with stand-in binaries
     (`scripts/test-package-macos`). The app has no icon yet, and the
     Homebrew cask is still to write;
   - Linux: `scripts/package-linux` builds `ginka-<version>-linux-<arch>.tar.gz`
     — `ginka`, `ginka-app`, `ginka-daemon`, a desktop entry and
     `install.sh`, which installs into `~/.local` (or `$PREFIX`) and
     uninstalls; CI checks the archive with stand-in binaries
     (`scripts/test-package-linux`). Wayland and X11 still need a person
     to look;
   - Windows (best effort): an installer and a portable zip.
6. Publish the signed appcast that every platform's updater reads (N15)
   *(not built yet)*.
7. Publish the GitHub release with the changelog section as its notes.

A nightly channel from `main` will use the same steps with a
`-nightly.<date>` version and its own appcast.
