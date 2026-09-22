#!/usr/bin/env bash
#
# Build and install tsk.
#
#   ./install.sh                 # install to ~/.local/bin
#   ./install.sh --prefix /usr/local   # install to /usr/local/bin (needs sudo)
#   ./install.sh --uninstall     # remove the installed binary
#   ./install.sh --debug         # faster build, larger/slower binary
#
set -euo pipefail

BIN_NAME="tsk"
PREFIX="${PREFIX:-$HOME/.local}"
PROFILE="release"
UNINSTALL=0

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix)    PREFIX="${2:?--prefix needs a directory}"; shift 2 ;;
        --prefix=*)  PREFIX="${1#*=}"; shift ;;
        --debug)     PROFILE="debug"; shift ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help)   sed -n '3,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *)           echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    esac
done

BIN_DIR="$PREFIX/bin"
TARGET="$BIN_DIR/$BIN_NAME"

say()  { printf '\033[1m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[33mwarning:\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31merror:\033[0m %s\n' "$*" >&2; exit 1; }

if [ "$UNINSTALL" -eq 1 ]; then
    if [ -e "$TARGET" ]; then
        rm -f "$TARGET"
        say "removed $TARGET"
        say "your data in ~/.local/share/tsk and config in ~/.config/tsk were NOT touched"
    else
        say "nothing to remove at $TARGET"
    fi
    exit 0
fi

cd "$(dirname "$0")"

# --- toolchain -----------------------------------------------------------
if ! command -v cargo >/dev/null 2>&1; then
    # rustup's default install location, in case the shell profile isn't loaded
    if [ -x "$HOME/.cargo/bin/cargo" ]; then
        PATH="$HOME/.cargo/bin:$PATH"
    else
        die "cargo not found. Install Rust with:
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  Debian's apt 'rustc' is too old for this project's dependencies."
    fi
fi

# --- build dependencies --------------------------------------------------
# SQLCipher and OpenSSL are compiled from vendored sources and statically
# linked, which is what lets one binary run on Debian and Mint alike. That
# build needs a C compiler and perl.
missing=""
for tool in cc make perl; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
done
if [ -n "$missing" ]; then
    die "missing build tools:$missing
  On Debian/Ubuntu/Mint:  sudo apt install build-essential perl"
fi

# --- build ---------------------------------------------------------------
if [ "$PROFILE" = "release" ]; then
    say "building $BIN_NAME (release; vendored SQLCipher + OpenSSL, takes ~1 min)"
    cargo build --release
    BUILT="target/release/$BIN_NAME"
else
    say "building $BIN_NAME (debug)"
    cargo build
    BUILT="target/debug/$BIN_NAME"
fi
[ -f "$BUILT" ] || die "build reported success but $BUILT is missing"

# --- install -------------------------------------------------------------
mkdir -p "$BIN_DIR"
if ! install -m 755 "$BUILT" "$TARGET" 2>/dev/null; then
    die "could not write $TARGET
  For a system prefix, re-run with sudo:  sudo ./install.sh --prefix ${PREFIX}"
fi
say "installed $TARGET ($(du -h "$TARGET" | cut -f1))"

# --- post-install checks -------------------------------------------------
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) warn "$BIN_DIR is not on your PATH. Add to ~/.bashrc:
    export PATH=\"$BIN_DIR:\$PATH\"" ;;
esac

if ldd "$TARGET" 2>/dev/null | grep -qE 'libssl|libcrypto|libsqlite3'; then
    warn "binary links system OpenSSL/SQLite; it may not run on other distributions"
fi

if [ -n "${TMUX:-}" ] || command -v tmux >/dev/null 2>&1; then
    tmux_term=$(tmux show -gv default-terminal 2>/dev/null || true)
    if [ -n "$tmux_term" ] && ! tmux show -gv terminal-overrides 2>/dev/null | grep -q "$tmux_term"; then
        warn "tmux truecolor is probably not reaching apps. tsk degrades to 256 colours.
  To enable it, add to ~/.tmux.conf:
    set -ga terminal-overrides \",${tmux_term}:RGB\""
    fi
fi

say "done. Run: $BIN_NAME"
say "data:   \${XDG_DATA_HOME:-~/.local/share}/tsk"
say "config: \${XDG_CONFIG_HOME:-~/.config}/tsk/config.toml"
