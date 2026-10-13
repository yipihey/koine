#!/usr/bin/env bash
# scripts/release/sign-ipa.sh - from a dx-built iOS .app to a TestFlight-ready .ipa
# (docs/plan-koine-mobile.md M-D8, docs/mobile.md "Onto a phone").
#
# Two subcommands, both run on a Mac with Xcode after `fastlane ios signing`
# has put the distribution certificate in the keychain and the App Store
# provisioning profile on disk:
#
#   entitlements --profile <.mobileprovision> --out <entitlements.plist>
#       The profile's Entitlements dictionary as a plist, for
#       `dx build --apple-entitlements`.
#
#   ipa --app <X.app> --profile <.mobileprovision> --identity "<Apple Distribution: …>"
#       --bundle-id <id> --version <x.y.z> --build <n> --out <X.ipa> [--icon <1024.png>]
#       Embeds the profile, stamps CFBundleShortVersionString / CFBundleVersion
#       (TestFlight refuses a build number it has seen) and
#       ITSAppUsesNonExemptEncryption=false (so the build does not wait on the
#       export-compliance question), adds the keys Xcode writes and App Store
#       validation requires (DTPlatformName, DTSDKName, DTXcode, MinimumOSVersion
#       from the binary, ...), compiles --icon into an asset catalog with actool
#       (CFBundleIconName; validation refuses an iOS bundle without one),
#       re-signs the bundle with the profile's entitlements, verifies the
#       signature, and zips Payload/ into the .ipa. Prints the .ipa path.
#
# Needs bash, PlistBuddy, security, codesign, zip, xcrun, vtool, actool (all on
# a Mac with Xcode).
set -euo pipefail

die() { echo "sign-ipa: $*" >&2; exit 1; }

entitlements_of() {
    # $1 profile, $2 out
    local decoded
    decoded="$(mktemp)"
    security cms -D -i "$1" >"$decoded" || die "could not decode the profile $1"
    /usr/libexec/PlistBuddy -x -c 'Print :Entitlements' "$decoded" >"$2" \
        || die "the profile $1 carries no Entitlements"
    rm -f "$decoded"
}

cmd="${1:-}"; shift || true
case "$cmd" in
entitlements)
    PROFILE=""; OUT=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --profile) PROFILE="$2"; shift 2 ;;
            --out) OUT="$2"; shift 2 ;;
            *) die "unknown argument $1" ;;
        esac
    done
    [[ -f "$PROFILE" && -n "$OUT" ]] || die "entitlements needs --profile <file> --out <file>"
    entitlements_of "$PROFILE" "$OUT"
    echo "$OUT"
    ;;
ipa)
    APP=""; PROFILE=""; IDENTITY=""; BUNDLE_ID=""; VERSION=""; BUILD=""; OUT=""; ICON=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --app) APP="$2"; shift 2 ;;
            --icon) ICON="$2"; shift 2 ;;
            --profile) PROFILE="$2"; shift 2 ;;
            --identity) IDENTITY="$2"; shift 2 ;;
            --bundle-id) BUNDLE_ID="$2"; shift 2 ;;
            --version) VERSION="$2"; shift 2 ;;
            --build) BUILD="$2"; shift 2 ;;
            --out) OUT="$2"; shift 2 ;;
            *) die "unknown argument $1" ;;
        esac
    done
    [[ -d "$APP" ]] || die "--app $APP is not a bundle"
    [[ -f "$PROFILE" ]] || die "--profile $PROFILE is not a file"
    [[ -n "$IDENTITY" && -n "$BUNDLE_ID" && -n "$VERSION" && -n "$BUILD" && -n "$OUT" ]] \
        || die "ipa needs --app --profile --identity --bundle-id --version --build --out"

    PLIST="$APP/Info.plist"
    [[ -f "$PLIST" ]] || die "$APP has no Info.plist"
    got="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$PLIST")"
    [[ "$got" == "$BUNDLE_ID" ]] || die "the bundle says CFBundleIdentifier=$got, not $BUNDLE_ID"

    set_or_add() { # key type value
        /usr/libexec/PlistBuddy -c "Set :$1 $3" "$PLIST" 2>/dev/null \
            || /usr/libexec/PlistBuddy -c "Add :$1 $2 $3" "$PLIST"
    }
    set_or_add CFBundleShortVersionString string "$VERSION"
    set_or_add CFBundleVersion string "$BUILD"
    set_or_add ITSAppUsesNonExemptEncryption bool false
    set_or_add CFBundlePackageType string APPL

    # What Xcode writes and App Store validation reads: the SDK and Xcode the
    # bundle was built with, and the deployment floor, which is the binary's.
    exe="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$PLIST")"
    [[ -f "$APP/$exe" ]] || die "$APP has no executable $exe"
    minos="$(xcrun vtool -show-build "$APP/$exe" | awk '/^ *minos /{print $2; exit}')"
    [[ -n "$minos" ]] || die "could not read minos from $APP/$exe"
    sdk_version="$(xcrun --sdk iphoneos --show-sdk-version)"
    sdk_build="$(xcrun --sdk iphoneos --show-sdk-build-version)"
    xcode_version="$(xcodebuild -version | awk '/^Xcode /{print $2}')"
    xcode_build="$(xcodebuild -version | awk '/^Build version /{print $3}')"
    IFS=. read -r xc_major xc_minor xc_patch <<<"$xcode_version"
    dt_xcode="$(printf '%02d%d%d' "$xc_major" "${xc_minor:-0}" "${xc_patch:-0}")"
    set_or_add MinimumOSVersion string "$minos"
    set_or_add DTPlatformName string iphoneos
    set_or_add DTPlatformVersion string "$sdk_version"
    set_or_add DTPlatformBuild string "$sdk_build"
    set_or_add DTSDKName string "iphoneos$sdk_version"
    set_or_add DTSDKBuild string "$sdk_build"
    set_or_add DTXcode string "$dt_xcode"
    set_or_add DTXcodeBuild string "$xcode_build"
    set_or_add DTCompiler string com.apple.compilers.llvm.clang.1_0
    set_or_add BuildMachineOSBuild string "$(sw_vers -buildVersion)"
    # dx lists "iPadOS", which is not a value Apple knows; Xcode writes iPhoneOS only.
    /usr/libexec/PlistBuddy -c 'Delete :CFBundleSupportedPlatforms' "$PLIST" 2>/dev/null || true
    /usr/libexec/PlistBuddy -c 'Add :CFBundleSupportedPlatforms array' "$PLIST"
    /usr/libexec/PlistBuddy -c 'Add :CFBundleSupportedPlatforms:0 string iPhoneOS' "$PLIST"

    if [[ -n "$ICON" ]]; then
        [[ -f "$ICON" ]] || die "--icon $ICON is not a file"
        catalog="$(mktemp -d)/Assets.xcassets"
        mkdir -p "$catalog/AppIcon.appiconset"
        cp "$ICON" "$catalog/AppIcon.appiconset/icon.png"
        cat >"$catalog/AppIcon.appiconset/Contents.json" <<'JSON'
{"images":[{"filename":"icon.png","idiom":"universal","platform":"ios","size":"1024x1024"}],
 "info":{"author":"xcode","version":1}}
JSON
        partial="$(mktemp -t icon-plist).plist"
        xcrun actool --compile "$APP" --platform iphoneos --target-device iphone --target-device ipad \
            --minimum-deployment-target "$minos" --app-icon AppIcon \
            --output-partial-info-plist "$partial" --output-format human-readable-text \
            "$catalog" >&2 || die "actool could not compile the icon"
        /usr/libexec/PlistBuddy -c "Merge $partial" "$PLIST" || die "could not merge the icon keys"
        rm -rf "$partial" "$(dirname "$catalog")"
    else
        echo "sign-ipa: no --icon; App Store validation refuses a bundle without CFBundleIconName" >&2
    fi

    cp "$PROFILE" "$APP/embedded.mobileprovision"
    ENT="$(mktemp -t entitlements).plist"
    entitlements_of "$PROFILE" "$ENT"

    # The profile's application-identifier must be this bundle (or a wildcard).
    app_id="$(/usr/libexec/PlistBuddy -c 'Print :application-identifier' "$ENT")"
    case "$app_id" in
        *".$BUNDLE_ID"|*".*") ;;
        *) die "the profile is for $app_id, not for $BUNDLE_ID" ;;
    esac

    codesign --force --timestamp=none --entitlements "$ENT" --sign "$IDENTITY" "$APP" \
        || die "codesign refused the bundle"
    codesign --verify --deep --strict --verbose=2 "$APP" >&2 \
        || die "the signed bundle does not verify"

    case "$OUT" in /*) ;; *) OUT="$PWD/$OUT" ;; esac
    work="$(mktemp -d)"
    mkdir -p "$work/Payload"
    cp -R "$APP" "$work/Payload/"
    rm -f "$OUT"
    ( cd "$work" && zip -qr "$OUT" Payload ) || die "zip failed"
    rm -rf "$work" "$ENT"
    echo "$OUT"
    ;;
*)
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2
    exit 1
    ;;
esac
