#!/bin/sh
# Install Ginka from this unpacked archive into ~/.local (or $PREFIX), or
# remove it again with `./install.sh uninstall`.
#
# Three binaries go into $PREFIX/bin — `ginka` (the command line), `ginka-app`
# (the window) and `ginka-daemon`, which the other two start and find beside
# themselves — and a desktop entry into $PREFIX/share/applications. Nothing
# needs root. Uninstalling leaves ~/.ginka, the state, where it is.
set -eu

PREFIX="${PREFIX:-$HOME/.local}"
here=$(cd "$(dirname "$0")" && pwd)
binaries="ginka ginka-app ginka-daemon"
desktop="$PREFIX/share/applications/ginka.desktop"

case "${1:-install}" in
install)
    mkdir -p "$PREFIX/bin" "$PREFIX/share/applications"
    for binary in $binaries; do
        cp "$here/bin/$binary" "$PREFIX/bin/$binary.new"
        chmod 0755 "$PREFIX/bin/$binary.new"
        # Replaced in one step, so a running daemon keeps its old file.
        mv -f "$PREFIX/bin/$binary.new" "$PREFIX/bin/$binary"
    done
    sed "s|@BIN@|$PREFIX/bin|" "$here/share/applications/ginka.desktop" >"$desktop"
    echo "Installed Ginka into $PREFIX/bin"
    case ":$PATH:" in
    *":$PREFIX/bin:"*) ;;
    *) echo "Add $PREFIX/bin to PATH to run \`ginka\` from a terminal." ;;
    esac
    ;;
uninstall)
    for binary in $binaries; do
        rm -f "$PREFIX/bin/$binary"
    done
    rm -f "$desktop"
    echo "Removed Ginka from $PREFIX; ~/.ginka is left as it was."
    ;;
*)
    echo "usage: $0 [install|uninstall]" >&2
    exit 2
    ;;
esac
