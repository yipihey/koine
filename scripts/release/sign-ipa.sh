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
#       --bundle-id <id> --version <x.y.z> --build <n> --out <X.ipa>
#       Embeds the profile, stamps CFBundleShortVersionString / CFBundleVersion
#       (TestFlight refuses a build number it has seen) and
#       ITSAppUsesNonExemptEncryption=false (so the build does not wait on the
#       export-compliance question), re-signs the bundle with the profile's
#       entitlements, verifies the signature, and zips Payload/ into the .ipa.
#       Prints the .ipa path.
#
# Needs bash, PlistBuddy, security, codesign, zip (all on a Mac with Xcode).
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
    APP=""; PROFILE=""; IDENTITY=""; BUNDLE_ID=""; VERSION=""; BUILD=""; OUT=""
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --app) APP="$2"; shift 2 ;;
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

    work="$(mktemp -d)"
    mkdir -p "$work/Payload"
    cp -R "$APP" "$work/Payload/"
    rm -f "$OUT"
    ( cd "$work" && zip -qr "$OLDPWD/$OUT" Payload ) || die "zip failed"
    rm -rf "$work" "$ENT"
    echo "$OUT"
    ;;
*)
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//' >&2
    exit 1
    ;;
esac
