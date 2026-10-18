#!/usr/bin/env bash
set -euo pipefail

project_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
package_name=nemuri
spec_file="$project_dir/packaging/$package_name.spec"
topdir="$project_dir/packaging/rpmbuild"
target_dir="${CARGO_TARGET_DIR:-$topdir/cargo-target}"

for command_name in cargo tar rpmbuild; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "error: required command not found: $command_name" >&2
        exit 1
    fi
done

# Keep Cargo artifacts outside BUILD, which rpmbuild removes after packaging.
# Resolve relative paths before rpmbuild changes into its source directory.
mkdir -p "$target_dir"
target_dir=$(cd "$target_dir" && pwd)

cargo_version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -n 1)
spec_version=$(sed -n 's/^Version:[[:space:]]*//p' "$spec_file" | head -n 1)
if [[ -z "$cargo_version" || "$cargo_version" != "$spec_version" ]]; then
    echo "error: Cargo.toml version '$cargo_version' does not match spec version '$spec_version'" >&2
    exit 1
fi
if [[ ! -f "$project_dir/Cargo.lock" ]]; then
    echo "error: Cargo.lock is required for a locked RPM build" >&2
    exit 1
fi

source_name="$package_name-$cargo_version"
source_archive="$project_dir/packaging/$source_name.tar.gz"
vendor_archive="$project_dir/packaging/$source_name-vendor.tar.gz"
work_dir="$topdir/work"

mkdir -p "$topdir/BUILD" "$topdir/BUILDROOT" "$topdir/RPMS" "$topdir/SOURCES" "$topdir/SPECS" "$topdir/SRPMS" "$work_dir"
rm -rf "$work_dir/$source_name" "$work_dir/vendor" "$work_dir/vendor-config.toml"

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
    packaging/nemuri.spec \
    packaging/build-rpm.sh \
    packaging/licenses \
    | tar -C "$work_dir/$source_name" -xf -
tar -C "$work_dir" -czf "$source_archive" "$source_name"

(cd "$project_dir" && cargo vendor --quiet --locked --versioned-dirs "$work_dir/vendor" >/dev/null)
printf '%s\n' \
    '[source.crates-io]' \
    'replace-with = "vendored-sources"' \
    '' \
    '[source.vendored-sources]' \
    'directory = "vendor"' > "$work_dir/vendor-config.toml"
tar -C "$work_dir" -czf "$vendor_archive" vendor vendor-config.toml

cp "$source_archive" "$vendor_archive" "$topdir/SOURCES/"
cp "$spec_file" "$topdir/SPECS/"

rpmbuild -ba \
    --define "_topdir $topdir" \
    --define "nemuri_cargo_target_dir $target_dir" \
    --define "nemuri_cargo_vendor_dir $work_dir/vendor" \
    --define "dist .fc44" \
    "$topdir/SPECS/$package_name.spec"

echo "RPM artifacts:"
find "$topdir/RPMS" "$topdir/SRPMS" -type f -name '*.rpm' -print
