# Releasing e1

> Status: implementation in progress. A published GitHub Release triggers a
> signed, notarized build that attaches the DMG, ZIP and checksums to the
> release. The signed appcast and R2 synchronization are still pending.
> Last updated: 2026-09-28

This document defines the release contract for the standalone macOS app. The
views embedded in Ginka remain Rust library crates and are not distributed on
their own.

## 1. Distribution decision

The first supported distribution is a universal macOS app in a notarized disk
image attached to a GitHub Release. The same release also carries the archive
existing installations consume through Sparkle:

```text
e1-v0.1.0-macos-universal.dmg
e1-v0.1.0-macos-universal.zip
e1-SHA256SUMS
```

The disk image contains `e1.app` and an Applications-folder shortcut. It runs
on both Apple silicon and Intel. A package installer is unnecessary because the
app has one executable and keeps its mutable data under `~/.e1`; the reader can
install it by dragging it to Applications.

The Mac App Store and Homebrew cask are later distribution channels. Existing
installations update through Sparkle from the first supported release; the DMG
remains the artifact for a new installation. Sparkle's Ed25519 signature is the
update authenticity boundary. The Developer ID signature, notarization and
HTTPS remain independently necessary, but none substitutes for that signature.

## 2. Automatic update contract

The implementation follows [Waku's release and update
shape](https://github.com/egoist/waku/blob/main/RELEASING.md): embed a pinned
Sparkle framework, publish a signed appcast and ZIP archives, keep old archives
available for binary deltas, and let Sparkle own download, verification,
replacement and relaunch. e1 deliberately starts with Sparkle's standard UI
instead of Waku's custom user driver. A custom in-window presentation is useful
only after the release path itself has survived an update from an older build.

### 2.1 Runtime boundary

The updater belongs to the standalone application, not to `e1-views`.
`e1-views` is mounted inside Ginka and must never acquire the ability to
replace its host application. The macOS bridge therefore lives in a small
`e1-updater-macos` crate used only by the root binary. Its safe public surface is
limited to initialization, an explicit check and Sparkle's persisted automatic
check preference.

Sparkle is loaded from
`e1.app/Contents/Frameworks/Sparkle.framework` at runtime. Initialization
returns no updater when the framework is absent, when the executable is not in
a supported app bundle, or in a debug build. Consequently `cargo run` cannot
offer to replace itself with a production build. `E1_FORCE_UPDATER=1` may
enable the real bridge in a development bundle for integration testing, but it
does not make a bare binary updateable.

The workspace denies unsafe Rust. Objective-C messaging and loading the
framework necessarily cross an unsafe FFI boundary, so that exception is
contained in `e1-updater-macos`; every unsafe operation documents the lifetime
and main-thread condition it relies on. No unsafe code enters `e1-views`,
`e1-ui` or `e1-github`.

On startup the application creates the updater and registers **Check for
Updates...** in the application menu only when initialization succeeds. The
menu action uses Sparkle's standard user-initiated window. Sparkle owns its
first-run consent prompt and automatic-check preference. Once the reader has
enabled automatic checks, e1 requests one silent background check per launch
and then leaves the schedule to Sparkle.

A later UI pass may expose three states in the standalone `Shell`: idle,
available and updating. An automatic result may then appear as an update button
in the sidebar footer, while an explicit menu action continues to use Sparkle's
standard window. The state is passed into `Shell` by the root binary; it is not
a global assumed by embeddable views.

### 2.2 Bundle and signing

The packaging script pins the Sparkle version and the SHA-256 digest of its
upstream distribution together. It downloads and caches that distribution,
copies `Sparkle.framework` into the app and removes development-only headers
and modules. e1 is not App-Sandboxed, so unused Sparkle XPC services are also
removed.

`Info.plist` contains:

- `SUFeedURL`, an HTTPS URL for the stable `appcast.xml` location; and
- `SUPublicEDKey`, the public half of e1's dedicated Sparkle Ed25519 key.

The private half never enters the repository or an artifact. A developer keeps
it in the login keychain; CI receives it as `SPARKLE_PRIVATE_KEY` from the
protected `release` environment and passes it to `generate_appcast` over
standard input. The release job verifies that every appcast enclosure has an
Ed25519 signature, so a wrong or missing key cannot silently publish an
unusable feed. The private key needs an offline backup: losing it strands every
installed build that trusts its public half.

Nested Sparkle executables are signed first, followed by the framework, e1's
executable and the app bundle. They use the same Developer ID identity because
Hardened Runtime library validation otherwise rejects the embedded framework.
The app and DMG are notarized and stapled before the stapled app is archived as
the ZIP Sparkle installs.

### 2.3 Publication topology

GitHub Releases remain the release record and the manual publication gate.
Cloudflare R2 is the update-serving surface because it gives the appcast a
stable URL, explicit cache policy and durable access to old archives. A custom
release domain can front the bucket later without changing the contract.

A published GitHub Release triggers a separate sync workflow. It uploads
versioned ZIPs, release notes and generated delta files with immutable cache
headers, then uploads `appcast.xml` last with a short cache lifetime. Publishing
the feed last ensures it never points at an archive that is not yet reachable.
Old ZIPs remain in the bucket so `generate_appcast` can build deltas for recent
versions and far-behind installations can fall back to a full archive.

The DMG and ZIP serve different readers:

- the notarized DMG is downloaded by a person installing e1; and
- the notarized ZIP and any deltas are referenced only by the signed appcast.

The feed and archive names are permanent once published. A failed sync may be
retried, but a versioned archive is never replaced with different bytes.

## 3. Trust model

Direct macOS distribution has three separate trust mechanisms:

1. A `Developer ID Application` certificate signs the app and disk image.
2. Apple's notary service scans the signed disk image and issues a ticket. The
   ticket is stapled to the disk image so installation also works when the
   notary service cannot be reached.
3. GitHub serves the artifacts over HTTPS. `SHA256SUMS` lets a reader detect an
   incomplete or substituted download when they obtain the expected digest
   through a trusted path.

Let's Encrypt issues TLS server certificates. It cannot issue an Apple
Developer ID code-signing certificate, submit an app to Apple's notary service,
or make Gatekeeper identify the publisher. It is useful only if e1 later owns a
download website; GitHub Releases already supplies HTTPS.

Unsigned builds remain useful for contributors, but they are development
artifacts. A public build without Developer ID and notarization makes readers
bypass Gatekeeper and is not a supported release.

## 4. One-time Apple and update setup

Before the first preview release:

- Enrol the publisher in the Apple Developer Program.
- Register and then freeze the bundle identifier. The proposed identifier is
  `com.bokuweb.e1`; changing it later creates a different application identity
  and can also affect Keychain behaviour.
- Create a `Developer ID Application` certificate. Export the identity and its
  private key as a password-protected PKCS#12 file for CI. A `Developer ID
  Installer` certificate is not needed while e1 ships as a DMG rather than a
  signed installer package.
- Create a dedicated App Store Connect API key for notarization and record its
  key ID, issuer ID and private `.p8` key.
- Create a protected GitHub Actions environment named `release`. Limit it to
  version tags and require a maintainer's approval before its secrets become
  available.
- Create the R2 bucket and credentials used only to write the release prefix.
- Generate a dedicated Sparkle Ed25519 key pair, put its public half in
  `Info.plist`, store its private half in the release environment, and keep an
  offline recovery copy.

The release environment holds these secrets:

| Secret | Contents |
| --- | --- |
| `APPLE_CERTIFICATE_P12` | Base64-encoded PKCS#12 signing identity |
| `APPLE_CERTIFICATE_PASSWORD` | Password for that PKCS#12 file |
| `APPLE_SIGNING_IDENTITY` | Full `Developer ID Application: ... (TEAMID)` name |
| `APPLE_NOTARY_KEY_P8` | Contents of the App Store Connect API private key |
| `APPLE_NOTARY_KEY_ID` | API key ID |
| `APPLE_NOTARY_ISSUER_ID` | API issuer ID |
| `APPLE_TEAM_ID` | Apple developer team ID, used for validation and diagnostics |
| `SPARKLE_PRIVATE_KEY` | Sparkle Ed25519 private key, supplied to `generate_appcast` over stdin |
| `R2_ACCESS_KEY_ID` | Access key for the release bucket |
| `R2_SECRET_ACCESS_KEY` | Secret key for the release bucket |
| `R2_ENDPOINT` | Account-specific R2 S3 endpoint |
| `R2_BUCKET` | Bucket that serves update archives and the appcast |

The certificate and notary key are different credentials. Keep both out of the
repository and action artifacts. Import the certificate into an ephemeral
keychain on the hosted runner, unlock that keychain only for the signing step,
and delete it in an `always()` cleanup step.

## 5. Bundle contract

Packaging is an explicit repository script rather than hidden in an IDE. It
constructs this bundle:

```text
e1.app/
└── Contents/
    ├── Info.plist
    ├── Frameworks/Sparkle.framework
    ├── MacOS/e1
    └── Resources/AppIcon.icns
```

The SVG used inside the UI is not an application icon. An `.icns` with all
required representations must be designed and checked into `assets/macos/`
before the first release.

`Info.plist` is generated from a checked-in template and contains at least:

- `CFBundleIdentifier = com.bokuweb.e1`
- `CFBundleExecutable = e1`
- `CFBundleName` and `CFBundleDisplayName = e1`
- `CFBundlePackageType = APPL`
- `CFBundleShortVersionString = <Cargo package version>`
- `CFBundleVersion = major * 1,000,000 + minor * 1,000 + patch`
- `LSMinimumSystemVersion = 11.0`
- `CFBundleIconFile = AppIcon`
- `NSHighResolutionCapable = true`

Ginka's root Rust toolchain and `Cargo.lock` are part of the build
input. CI builds with `--locked` and sets `MACOSX_DEPLOYMENT_TARGET=11.0`
explicitly for both slices. The current arm64 binary already declares macOS
11.0; making it explicit prevents the Intel slice or a future runner image from
silently choosing another floor.

The first release has no entitlements. It needs network and Keychain access but
is not App-Sandboxed, and it has no JIT, debugger, camera, microphone, location
or Apple Events requirement. Add an entitlement only when a concrete feature
needs it, with a signed-bundle smoke test. Hardened Runtime is enabled by
`codesign --options runtime`, with a secure timestamp.

Build the local development artifacts with:

```bash
e1/scripts/bundle-macos.sh debug
```

This writes the ad-hoc signed app, update ZIP and DMG under `target/dist/`.
Release mode requires `E1_CODESIGN_IDENTITY`. Set both
`E1_SPARKLE_FEED_URL` and `E1_SPARKLE_PUBLIC_KEY` to include Sparkle; without
them the signed bundle omits the updater and its feed credentials. After placing the ZIP
and its matching release-notes file in an updates directory, generate the
signed feed with `e1/scripts/appcast-macos.sh <directory>`. The signing key comes
from `SPARKLE_PRIVATE_KEY` over stdin or from Sparkle's login-keychain entry.
CI may set `E1_ALLOW_ADHOC_RELEASE=1` with `E1_CODESIGN_IDENTITY=-` only for
the secret-free universal-build dry run; the public release path never sets it.

## 6. Version and tag contract

Ginka's root workspace package version is the source of truth. Stable releases use
three-integer SemVer versions initially; for example, Cargo version `0.1.0`
maps to tag `v0.1.0`, short app version `0.1.0` and bundle version `1000`.
Minor and patch components must each fit in three decimal digits. This mapping
keeps Sparkle's build-number comparison monotonic and reproducible. A release
job refuses a tag that does not exactly match the Cargo version.

Every release starts as a normal release-preparation pull request that:

1. updates the root workspace package version inherited by e1;
2. updates `Cargo.lock`;
3. moves user-visible changes into a versioned changelog section;
4. passes the ordinary pull-request CI; and
5. records any compatibility or migration note.

After merge, a maintainer creates and pushes the annotated `vX.Y.Z` tag. Tags
are immutable: a failed release is rerun for the same commit, while changed
code receives a new version. The workflow creates a draft GitHub Release; a
maintainer publishes it only after the installation checks pass.

Prerelease version mapping needs a separate decision before the first beta:
Apple's bundle version fields are more restrictive than Cargo SemVer. Do not
invent a mapping inside the workflow.

## 7. Automated release jobs

### Current artifact workflow

Ginka's `.github/workflows/e1-release.yml` runs when a Ginka GitHub Release is
published. It checks that the release tag matches the root Cargo version,
builds both macOS architectures, signs the app, notarizes and staples the app
and DMG, then uploads the final DMG, ZIP and checksums as one Actions artifact
and attaches each file to the GitHub Release. It does not generate an appcast.
This is the interim signed artifact path; the tag-driven draft and publish-triggered R2
jobs described below remain to be implemented.

Configure Actions secrets `MACOS_CERTIFICATE_P12_BASE64`,
`MACOS_CERTIFICATE_PASSWORD`, `APPLE_ID` and
`APPLE_APP_SPECIFIC_PASSWORD`. Repository variables
`E1_SPARKLE_FEED_URL` and `E1_SPARKLE_PUBLIC_KEY` are optional, but must be
set together to include the updater. Use a real HTTPS feed URL and its matching
Sparkle public key. The local `ccs-notarization` keychain profile is not
available on a GitHub runner. The GitHub Release tag must be `v` followed by
the root Cargo version.

### Planned publication workflow

Add `.github/workflows/release.yml`, triggered only by `v*` tag pushes and also
available as a dry-run `workflow_dispatch` that cannot access release secrets.
The signed path runs on a GitHub-hosted macOS runner in the protected `release`
environment with `contents: write` and no broader repository permissions.

The job is deliberately linear after compilation because each artifact is the
input to the next trust step:

```text
validate tag/version and clean source
        │
        ├─ cargo test --workspace --locked
        ├─ cargo clippy --workspace --all-targets --locked -- -D warnings
        └─ build release slices (arm64 + x86_64)
                            │
                         lipo -create
                            │
                    construct e1.app
                            │
           sign app (Developer ID + runtime + timestamp)
                            │
              verify signature and both architectures
                            │
                    construct and sign DMG
                            │
               notarytool submit --wait
                            │
                  staple and validate ticket
                            │
          mount DMG; Gatekeeper and launch smoke tests
                            │
       staple app; archive ZIP; generate candidate appcast
                            │
          checksum; create draft GitHub Release
```

Use `cargo build --release --locked` for `aarch64-apple-darwin` and
`x86_64-apple-darwin`, then `lipo -create` the two `e1` binaries. Do not build
one slice on a developer machine and the other in CI: both must come from the
tagged tree, pinned toolchain and one controlled job.

Sign nested code first and the app bundle last. e1 initially contains only one
Mach-O, so this is just the executable followed by the bundle; the rule matters
when helpers or frameworks arrive. Avoid `codesign --deep` as a signing
strategy. Verify with `codesign --verify --deep --strict --verbose=2` and inspect
the identity, Team ID, hardened-runtime flag, entitlements and architectures.

Submit the signed DMG with `xcrun notarytool submit --wait` using the API key.
On either success or failure, retain the submission ID and download the notary
log; success with warnings is still actionable. On success, run `xcrun stapler
staple` and `xcrun stapler validate` on the DMG. Mount the final DMG read-only,
run `spctl --assess --type execute` against its app, copy the app to a temporary
directory and launch it once with `E1_DEMO=1`. This smoke test must use the
signed bundle rather than `cargo run`.

Generate `SHA256SUMS` only after stapling, because stapling changes the DMG and
app. Upload only the final DMG, ZIP, candidate appcast, checksums and release
notes. Intermediate unsigned apps, private keys, temporary keychains and
notarization upload archives must never become workflow artifacts.

Add `.github/workflows/sync-release.yml`, triggered when the draft is
published. It downloads the release artifacts, fetches a bounded history of
old ZIPs from R2, regenerates the signed appcast and deltas, verifies every
enclosure, uploads immutable files first and `appcast.xml` last. It has no
Developer ID or notarization credential; it receives only the Sparkle and R2
secrets it needs.

## 8. Checks before publishing the draft

CI proves the mechanical contract. A maintainer completes the release by
checking the draft on a second Mac account or machine:

- download the asset from the draft release rather than using the runner copy;
- verify its SHA-256 digest, mount it and drag `e1.app` to Applications;
- confirm Gatekeeper names the expected developer and opens without a bypass;
- sign in through GitHub's device flow, relaunch, and confirm the token is found
  in Keychain;
- open the inbox and one pull request, open a browser link, then sign out;
- repeat a launch without network access to cover the stapled ticket and cached
  startup; and
- on one Intel Mac for the first release, confirm the Intel slice actually runs.
- from an installed older build, choose **Check for Updates...**, install the
  draft candidate from a temporary feed, and confirm the replacement is the
  expected universal, signed and notarized version.

Publish the existing draft after those checks. If notarization or installation
fails, leave the draft unpublished and preserve the notary log in the workflow
log after inspecting it for secrets.

## 9. Failure and rotation rules

- A signing or notarization failure publishes nothing. There is no unsigned
  fallback release.
- The sync workflow uploads `appcast.xml` last. A failure before that point
  leaves existing installations on the previous valid feed.
- A release archive with a published URL is immutable. Changed bytes require a
  new version even when the preceding release cannot update successfully.
- A partially created draft is safe to delete; a published tag or release is
  never silently replaced.
- If the Developer ID private key may have leaked, revoke it with Apple, remove
  the GitHub secret, inspect issued/notarized builds, create a new certificate
  and release a newly versioned build.
- If only the notary API key leaks, revoke and replace that key. It cannot sign
  the app, but it still grants access to the team's notarization API.
- Renew Apple Developer Program membership and rotate credentials before they
  expire. Existing validly signed apps can continue to run after a normal
  certificate expiry, but new releases need a current certificate.

## 10. Implementation order

1. Decide the licence (roadmap Q3), publisher identity and final bundle ID.
2. Create the application icon and `Info.plist` template.
3. Add a local packaging script with ad-hoc signing, a pinned Sparkle framework
   and a DMG smoke test; keep all output under `target/dist/`.
4. Add `e1-updater-macos` and the standard **Check for Updates...** flow,
   dormant in ordinary debug builds.
5. Generate a signed ZIP and appcast locally, then prove an update from an
   older development bundle before automating publication.
6. Add a CI dry run that builds and verifies the universal unsigned/ad-hoc
   artifacts without release secrets.
7. Provision Apple, Sparkle, GitHub and R2 credentials.
8. Add signed tag releases, notarization and the manual publish gate.
9. Add the publish-triggered R2 sync, retaining old ZIPs and publishing the
   verified appcast last.
10. After the first production update succeeds, consider the sidebar status,
    tune the retained delta history, and add a settings toggle. A Homebrew
    cask remains independent.
