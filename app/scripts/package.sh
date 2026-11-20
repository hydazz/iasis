#!/bin/sh
# Stage an installer package from a built app: the app, the LaunchAgents
# that start it and its agent, and the `iasis` command.
#
#   package.sh <Iasis.app> <output.pkg> [<Developer ID Installer identity>]

set -eu

app="$1"
output="$2"
identity="${3:-}"
here="$(cd "$(dirname "$0")/.." && pwd)"
version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "${app}/Contents/Info.plist")"

stage="$(mktemp -d)"
trap 'rm -rf "${stage}"' EXIT

/usr/bin/ditto "${here}/packaging/root" "${stage}"
/bin/mkdir -p "${stage}/Applications"
/usr/bin/ditto "${app}" "${stage}/Applications/Iasis.app"

set -- --root "${stage}" \
    --scripts "${here}/packaging/scripts" \
    --identifier com.hydaz.Iasis \
    --version "${version}" \
    --install-location /
if [ -n "${identity}" ]; then
    set -- "$@" --sign "${identity}"
fi
/usr/bin/pkgbuild "$@" "${output}"
