# Update Folio to the latest GitHub release. `folio --update` runs this with
# the running version as $1; the release's own install.sh does the install.
set -e
repo=nikosk/folio
current=$1
if command -v curl >/dev/null 2>&1; then
    get() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    get() { wget -qO- "$1"; }
else
    echo "folio --update needs curl or wget" >&2
    exit 1
fi
tag=$(get "https://api.github.com/repos/$repo/releases/latest" | sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n1)
[ -n "$tag" ] || { echo "Could not find the latest release" >&2; exit 1; }
latest=${tag#v}
if [ "$(printf '%s\n%s\n' "$current" "$latest" | sort -V | tail -n1)" = "$current" ]; then
    echo "Folio $current is up to date"
    exit 0
fi
name="folio-$latest-x86_64-linux"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
echo "Downloading Folio $latest…"
url="https://github.com/$repo/releases/download/$tag/$name.tar.gz"
get "$url" > "$tmp/$name.tar.gz"
get "$url.sha256" > "$tmp/$name.tar.gz.sha256"
(cd "$tmp" && sha256sum -c --quiet "$name.tar.gz.sha256")
tar -C "$tmp" -xzf "$tmp/$name.tar.gz"
"$tmp/$name/install.sh" >/dev/null
echo "Updated Folio $current → $latest. Restart Folio to use it."
