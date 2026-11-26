#!/bin/sh
# Install Display Lab.
#
#   curl -fsSL https://raw.githubusercontent.com/raymond-UI/display-lab/main/install.sh | sh
#
# Puts one file, `displaylab`, in ~/.local/bin. Run it again to update.
# To remove it: rm ~/.local/bin/displaylab
#
# Options, as environment variables:
#   DISPLAYLAB_VERSION       a release tag such as v0.2.0 (default: the latest)
#   DISPLAYLAB_INSTALL_DIR   where to put it (default: ~/.local/bin)
set -eu

repo="raymond-UI/display-lab"
version="${DISPLAYLAB_VERSION:-latest}"
dir="${DISPLAYLAB_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
fail() { say "displaylab install: $*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) asset="displaylab-macos.tar.gz" ;;
  Linux)
    case "$(uname -m)" in
      x86_64 | amd64) asset="displaylab-linux-x86_64.tar.gz" ;;
      *) fail "there is no Linux build for $(uname -m) yet" ;;
    esac
    ;;
  *) fail "on Windows, download displaylab-windows-x86_64.zip from https://github.com/$repo/releases" ;;
esac

if [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

command -v curl > /dev/null 2>&1 || fail "curl is needed"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "Downloading Display Lab ($version)..."
curl -fsSL -o "$tmp/$asset" "$base/$asset" || fail "could not download $base/$asset"
curl -fsSL -o "$tmp/$asset.sha256" "$base/$asset.sha256" || fail "could not download the checksum"

expected="$(cut -d ' ' -f 1 < "$tmp/$asset.sha256")"
if command -v shasum > /dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset" | cut -d ' ' -f 1)"
else
  actual="$(sha256sum "$tmp/$asset" | cut -d ' ' -f 1)"
fi
[ "$expected" = "$actual" ] || fail "the download does not match its checksum; try again"

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$dir"
mv -f "$tmp/displaylab" "$dir/displaylab"
chmod +x "$dir/displaylab"

say "Installed $("$dir/displaylab" --version) to $dir/displaylab"

case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    case "${SHELL:-}" in
      */zsh) rc="$HOME/.zshrc" ;;
      */bash) rc="$HOME/.bashrc" ;;
      *) rc="" ;;
    esac
    say ""
    say "$dir is not on your PATH yet."
    if [ -n "$rc" ]; then
      say "Add it with:"
      say "  echo 'export PATH=\"$dir:\$PATH\"' >> $rc && exec \$SHELL"
    else
      say "Add $dir to your PATH, then open a new terminal."
    fi
    ;;
esac

say ""
say "Try it:"
say "  displaylab run --backend fake catalogue   # no screens touched, nothing to set up"
if [ "$(uname -s)" = Darwin ]; then
  say "  displaylab run smoke                      # a real virtual projector on this Mac"
fi
