#!/bin/sh
# Install the published Forge preview for the current user.
set -eu

version=v0.2.0-preview.1
case "$(uname -s):$(uname -m)" in
    Linux:x86_64)
        asset=forge-Linux-X64.tar.gz
        checksum=70f1ad610ddd371e4e2c299b809970a6aa9e11ba80638ebdb81266dcf96db75d
        ;;
    Darwin:arm64)
        asset=forge-macOS-ARM64.tar.gz
        checksum=fa17fc85cbb31d8ef96363a6df0dc8b3845e087cae9705d10c535a2d6e077b5b
        ;;
    *)
        echo 'No published Forge binary for this platform. See the README to build from source.' >&2
        exit 1
        ;;
esac

bin_dir=${FORGE_INSTALL_DIR:-"$HOME/.local/bin"}
mkdir -p "$bin_dir"
stage=$(mktemp -d "$bin_dir/.forge-install.XXXXXXXX")
trap 'rm -f "$stage/archive" "$stage/forge"; rmdir "$stage"' EXIT
trap 'exit 1' HUP INT TERM

curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
    "https://github.com/N-Aymane/forge-harness/releases/download/$version/$asset" \
    --output "$stage/archive"

if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum < "$stage/archive")
else
    actual=$(shasum -a 256 < "$stage/archive")
fi
if [ "${actual%% *}" != "$checksum" ]; then
    echo 'Forge archive checksum mismatch.' >&2
    exit 1
fi

tar -xOzf "$stage/archive" forge > "$stage/forge"
chmod 755 "$stage/forge"
if [ -d "$bin_dir/forge" ]; then
    echo "Cannot install Forge: $bin_dir/forge is a directory." >&2
    exit 1
fi
mv -f "$stage/forge" "$bin_dir/forge"
printf 'Installed Forge %s to %s\n' "$version" "$bin_dir/forge"
