# Releasing

Ginka has not been released. This is the process the first release will
follow, written down before it happens (roadmap M6); the parts that do not
exist yet are marked as such.

## Versions

- One version for Ginka and e1, in the root `Cargo.toml` and inherited by the
  workspace crates, currently `0.0.0`.
- The wire protocol has its own number, `PROTOCOL_VERSION`, which moves with
  every change to the requests; a newer client replaces an older daemon, and
  never the other way round.

## Steps

1. Update `CHANGELOG.md`: move *Unreleased* under the new version and date.
2. Set the root and workspace package versions in `Cargo.toml`, run
   `cargo build --workspace` so `Cargo.lock` follows, and commit both with the
   changelog.
3. Check the tree is green: `cargo fmt --check`, `cargo clippy` with and
   without `--features github`, `cargo test --workspace`.
4. Tag the commit `vX.Y.Z` and push the tag.
5. Build the local artifacts:
   - macOS: `scripts/package-macos --universal` builds `Ginka.app` with the
     e1 GitHub Inbox, the window as its executable, and `ginka` and
     `ginka-daemon` beside it (N16), plus `Ginka-<version>.zip`,
     `Ginka-<version>.dmg`, and `SHA256SUMS`.
     Set `GINKA_SIGN_IDENTITY` to a Developer ID certificate to sign with the
     hardened runtime (ad hoc otherwise) and
     `GINKA_NOTARY_PROFILE` to a `notarytool` keychain profile to notarize
     and staple. CI checks the bundle with stand-in binaries
     (`scripts/test-package-macos`). The app has no icon yet, and the
     Homebrew cask is still to write;
   - standalone e1: `e1/scripts/bundle-macos.sh release` builds `e1.app`,
     `e1-v<version>-macos-universal.zip`, and the matching DMG. It requires
     `E1_CODESIGN_IDENTITY`. Set both `E1_SPARKLE_FEED_URL` and
     `E1_SPARKLE_PUBLIC_KEY` to include the Sparkle updater;
   - Linux: `scripts/package-linux` builds `ginka-<version>-linux-<arch>.tar.gz`
     with the e1 GitHub Inbox: `ginka`, `ginka-app`, `ginka-daemon`, a desktop entry and
     `install.sh`, which installs into `~/.local` (or `$PREFIX`) and
     uninstalls; CI checks the archive with stand-in binaries
     (`scripts/test-package-linux`). Wayland and X11 still need a person
     to look;
   - Windows (best effort): an installer and a portable zip.
6. Publish the signed appcast that every platform's updater reads (N15)
   *(not built yet)*.
7. Publish the GitHub release with the changelog section as its notes.
   Publishing a release whose tag matches the root package version runs
   `.github/workflows/release.yml` and `.github/workflows/e1-release.yml`.
   They build the universal Ginka and standalone e1 macOS apps, sign them
   with Developer ID, notarize and staple the apps and DMGs, then upload each
   app's ZIP, DMG, and checksums as workflow artifacts and attach them to the
   same GitHub Release. Configure Actions
   secrets `MACOS_CERTIFICATE_P12_BASE64`, `MACOS_CERTIFICATE_PASSWORD`, `APPLE_ID`,
   and `APPLE_APP_SPECIFIC_PASSWORD`; a local `notarytool` keychain profile
   is not available on the runner. Repository variables
   `E1_SPARKLE_FEED_URL` and `E1_SPARKLE_PUBLIC_KEY` are optional for e1.
   Without both, the standalone e1 artifact omits the updater.

A nightly channel from `main` will use the same steps with a
`-nightly.<date>` version and its own appcast.
