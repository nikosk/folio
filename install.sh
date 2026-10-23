#!/bin/sh
# Install Folio for the current user (~/.local).
# Works from a release tarball (uses the bundled binary) or a source checkout (builds it).
set -e
cd "$(dirname "$0")"
if [ -x ./folio ]; then
    bin=./folio
else
    cargo build --release
    bin=target/release/folio
fi
install -Dm755 "$bin" "$HOME/.local/bin/folio"
mkdir -p "$HOME/.local/share/applications"
sed "s|^Exec=folio|Exec=$HOME/.local/bin/folio|" packaging/dev.folio.Folio.desktop \
    > "$HOME/.local/share/applications/dev.folio.Folio.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "Installed to ~/.local/bin/folio"
echo "Make it the default for Markdown:  xdg-mime default dev.folio.Folio.desktop text/markdown"
