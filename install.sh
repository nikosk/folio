#!/bin/sh
# Build Folio and install it for the current user (~/.local).
set -e
cargo build --release
install -Dm755 target/release/folio "$HOME/.local/bin/folio"
sed "s|^Exec=folio|Exec=$HOME/.local/bin/folio|" packaging/dev.folio.Folio.desktop \
    > "$HOME/.local/share/applications/dev.folio.Folio.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
echo "Installed to ~/.local/bin/folio"
echo "Make it the default for Markdown:  xdg-mime default dev.folio.Folio.desktop text/markdown"
