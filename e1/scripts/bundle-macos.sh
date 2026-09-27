#!/usr/bin/env bash
# Build a self-contained macOS app, optionally embedding pinned Sparkle.
#
# Usage: e1/scripts/bundle-macos.sh [debug|release]
#
# A release build requires E1_CODESIGN_IDENTITY. Supply both Sparkle values
# to enable updates; otherwise the bundle has no update feed or key.

set -euo pipefail

script_directory=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
project_root=$(cd "$script_directory/.." && pwd)
workspace_root=$(cd "$project_root/.." && pwd)
profile=${1:-debug}

case "$profile" in
  debug | release) ;;
  *)
    echo "usage: e1/scripts/bundle-macos.sh [debug|release]" >&2
    exit 2
    ;;
esac

sparkle_version="2.9.4"
sparkle_sha256="ce89daf967db1e1893ed3ebd67575ed82d3902563e3191ca92aaec9164fbdef9"
sparkle_cache_root="${E1_BUILD_CACHE_DIR:-$project_root/.e1-cache}/sparkle"
sparkle_cache_entry="$sparkle_cache_root/$sparkle_version"
sparkle_framework_source="$sparkle_cache_entry/Sparkle.framework"

target_directory=${CARGO_TARGET_DIR:-$workspace_root/target}
dist_directory="$target_directory/dist"
bundle="$dist_directory/e1.app"
contents="$bundle/Contents"
framework="$contents/Frameworks/Sparkle.framework"
bundle_identifier=${E1_BUNDLE_ID:-com.bokuweb.e1}
feed_url=${E1_SPARKLE_FEED_URL:-https://example.invalid/e1/appcast.xml}
public_key=${E1_SPARKLE_PUBLIC_KEY:-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=}
codesign_identity=${E1_CODESIGN_IDENTITY:--}
updates_enabled=1

if [[ "$profile" == "release" ]]; then
  : "${E1_CODESIGN_IDENTITY:?release builds require E1_CODESIGN_IDENTITY}"
  if [[ -n "${E1_SPARKLE_FEED_URL:-}" || -n "${E1_SPARKLE_PUBLIC_KEY:-}" ]]; then
    if [[ -z "${E1_SPARKLE_FEED_URL:-}" || -z "${E1_SPARKLE_PUBLIC_KEY:-}" ]]; then
      echo "set both E1_SPARKLE_FEED_URL and E1_SPARKLE_PUBLIC_KEY to enable updates" >&2
      exit 1
    fi
  fi
  if [[ -z "${E1_SPARKLE_FEED_URL:-}" ]]; then
    updates_enabled=0
  fi
  if [[ "$codesign_identity" == "-" && "${E1_ALLOW_ADHOC_RELEASE:-}" != "1" ]]; then
    echo "release builds require a Developer ID identity; set E1_ALLOW_ADHOC_RELEASE=1 only for a dry run" >&2
    exit 1
  fi
fi

version=$(awk '
  /^\[package\]$/ { package = 1; next }
  /^\[/ { package = 0 }
  package && /^version = / {
    value = $0
    sub(/^version = "/, "", value)
    sub(/"$/, "", value)
    print value
    exit
  }
' "$workspace_root/Cargo.toml")

if [[ ! "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "Cargo package version must be a stable three-integer SemVer: $version" >&2
  exit 1
fi
major=${BASH_REMATCH[1]}
minor=${BASH_REMATCH[2]}
patch=${BASH_REMATCH[3]}
if (( minor > 999 || patch > 999 )); then
  echo "minor and patch versions must be at most 999" >&2
  exit 1
fi
bundle_version=$((major * 1000000 + minor * 1000 + patch))

if [[ "$updates_enabled" == 1 && ! -d "$sparkle_framework_source" ]]; then
  mkdir -p "$sparkle_cache_root"
  staging=$(mktemp -d "$sparkle_cache_root/.staging-$sparkle_version.XXXXXX")
  trap 'rm -rf "$staging"' EXIT
  archive="$staging/Sparkle-$sparkle_version.tar.xz"
  curl -fsSL --retry 3 \
    -o "$archive" \
    "https://github.com/sparkle-project/Sparkle/releases/download/$sparkle_version/Sparkle-$sparkle_version.tar.xz"
  echo "$sparkle_sha256  $archive" | shasum -a 256 -c - >/dev/null
  tar -xJf "$archive" -C "$staging" ./Sparkle.framework ./bin
  rm "$archive"
  mv "$staging" "$sparkle_cache_entry"
  trap - EXIT
fi

cd "$workspace_root"
if [[ "$profile" == "release" ]]; then
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked -p e1 --target aarch64-apple-darwin
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --release --locked -p e1 --target x86_64-apple-darwin
  mkdir -p "$dist_directory"
  lipo -create \
    "$target_directory/aarch64-apple-darwin/release/e1" \
    "$target_directory/x86_64-apple-darwin/release/e1" \
    -output "$dist_directory/e1-universal"
  executable="$dist_directory/e1-universal"
  artifact_platform="macos-universal"
else
  MACOSX_DEPLOYMENT_TARGET=11.0 cargo build --locked -p e1
  executable="$target_directory/debug/e1"
  artifact_platform="macos-$(uname -m)-debug"
fi

if [[ "$bundle" != "$dist_directory/e1.app" ]]; then
  echo "refusing to replace unexpected bundle path: $bundle" >&2
  exit 1
fi
rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources" "$contents/Frameworks"
cp "$executable" "$contents/MacOS/e1"
cp "$project_root/resources/macos/Info.plist" "$contents/Info.plist"
cp "$project_root/assets/macos/AppIcon.icns" "$contents/Resources/AppIcon.icns"
if [[ "$updates_enabled" == 1 ]]; then
  ditto "$sparkle_framework_source" "$framework"

  # e1 is not sandboxed. These services and development files are not used by
  # the standard updater and would add nested code that has to be shipped.
  for extra in XPCServices Headers PrivateHeaders Modules; do
    rm -rf "${framework:?}/$extra" "${framework:?}/Versions/B/$extra"
  done
fi

plutil -replace CFBundleIdentifier -string "$bundle_identifier" "$contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$version" "$contents/Info.plist"
plutil -replace CFBundleVersion -string "$bundle_version" "$contents/Info.plist"
if [[ "$updates_enabled" == 0 ]]; then
  plutil -remove SUFeedURL "$contents/Info.plist"
  plutil -remove SUPublicEDKey "$contents/Info.plist"
else
  plutil -replace SUFeedURL -string "$feed_url" "$contents/Info.plist"
  plutil -replace SUPublicEDKey -string "$public_key" "$contents/Info.plist"
fi
xattr -cr "$bundle"

sign_one() {
  local path=$1
  if [[ "$codesign_identity" == "-" ]]; then
    codesign --force --sign - "$path"
  else
    codesign --force --options runtime --timestamp --sign "$codesign_identity" "$path"
  fi
}

if [[ "$updates_enabled" == 1 ]]; then
  sign_one "$framework/Versions/B/Autoupdate"
  sign_one "$framework/Versions/B/Updater.app"
  sign_one "$framework"
fi
sign_one "$bundle"
codesign --verify --deep --strict --verbose=2 "$bundle"

archive="$dist_directory/e1-v$version-$artifact_platform.zip"
image="$dist_directory/e1-v$version-$artifact_platform.dmg"
rm -f "$archive" "$image"
ditto -c -k --sequesterRsrc --keepParent "$bundle" "$archive"

dmg_source=$(mktemp -d "$dist_directory/.dmg.XXXXXX")
trap 'rm -rf "$dmg_source"' EXIT
ditto "$bundle" "$dmg_source/e1.app"
ln -s /Applications "$dmg_source/Applications"
hdiutil create -quiet -volname e1 -srcfolder "$dmg_source" -ov -format UDZO "$image"
if [[ "$codesign_identity" != "-" ]]; then
  sign_one "$image"
fi
rm -rf "$dmg_source"
trap - EXIT

echo "Built $bundle"
echo "Built $archive"
echo "Built $image"
