#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
package_name=nemuri
pkgbuild_file="$project_dir/packaging/arch/PKGBUILD"
build_dir="$project_dir/packaging/archbuild"

for command_name in cargo tar makepkg; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "error: required command not found: $command_name" >&2
        exit 1
    fi
done

cargo_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -n 1)
pkgbuild_version=$(sed -n 's/^pkgver=//p' "$pkgbuild_file" | head -n 1)
if [[ -z "$cargo_version" || "$cargo_version" != "$pkgbuild_version" ]]; then
    echo "error: Cargo.toml version '$cargo_version' does not match PKGBUILD version '$pkgbuild_version'" >&2
    exit 1
fi
if [[ ! -f "$project_dir/Cargo.lock" ]]; then
    echo "error: Cargo.lock is required for a locked Arch build" >&2
    exit 1
fi

source_name="$package_name-$cargo_version"
work_dir="$build_dir/work"

mkdir -p "$build_dir"
rm -rf "$work_dir"

mkdir -p "$work_dir/$source_name"
tar -C "$project_dir" \
    -cf - \
    Cargo.toml \
    Cargo.lock \
    LICENSE \
    README.md \
    changelog.md \
    assets \
    src \
    tests \
    packaging/nemuri.1 \
    packaging/nemuri.desktop \
    packaging/arch/PKGBUILD \
    packaging/build-arch.sh \
    packaging/licenses \
    | tar -C "$work_dir/$source_name" -xf -
tar -C "$work_dir" -czf "$build_dir/$source_name.tar.gz" "$source_name"
rm -rf "$work_dir"

cp "$pkgbuild_file" "$build_dir/PKGBUILD"
(cd "$build_dir" && makepkg --force --cleanbuild)

echo "Arch packages:"
find "$build_dir" -maxdepth 1 -type f -name '*.pkg.tar.*' -print
