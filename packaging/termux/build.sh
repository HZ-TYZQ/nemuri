#!/usr/bin/env bash
# Cross-compile nemuri for Termux with the Android NDK.
#
#   deb    build the release binary and package it as a Termux .deb
#   tests  build the test executables, to run inside Termux (CI uses termux-docker)
#
# Two ways to get sound out:
#   aaudio  cpal's Android backend, which plays through AAudio (Android 8+). Its
#           JNI queries would panic without a JVM; nemuri hands it an empty
#           context so they fail and it falls back to defaults.
#   alsa    cpal patched to use its ALSA backend, like the cpal users in
#           termux-packages (termusic, chroma-visualizer). alsa-lib with
#           alsa-plugins plays through Termux's PulseAudio.
#
# Usage: packaging/termux/build.sh {deb|tests} [aarch64|x86_64|arm|i686] [aaudio|alsa]
# Prints the .deb path, or the directory holding the test executables.
# Needs the NDK in $ANDROID_NDK_HOME (default ~/android/android-ndk-r30), the
# Rust target for the arch, dpkg-deb, pkg-config and python3. Locally, run it
# inside the dev-fedora container.
set -euo pipefail

project_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
mode=${1:-}
arch=${2:-aarch64}
audio=${3:-aaudio}
termux_prefix=/data/data/com.termux/files/usr
repo_url=https://packages.termux.dev/apt/termux-main
ndk=${ANDROID_NDK_HOME:-$HOME/android/android-ndk-r30}

case "$mode" in
    deb | tests) ;;
    *)
        echo "usage: $0 {deb|tests} [aarch64|x86_64|arm|i686] [aaudio|alsa]" >&2
        exit 1
        ;;
esac

case "$audio" in
    # libaaudio only exists from API 26 on.
    aaudio)
        api=26
        link_packages=(libsqlite zlib)
        depends="libsqlite"
        ;;
    alsa)
        api=24
        link_packages=(alsa-lib libandroid-sysv-semaphore libandroid-shmem libsqlite zlib)
        depends="alsa-lib, alsa-plugins, pulseaudio, libsqlite"
        ;;
    *)
        echo "error: unknown audio backend '$audio' (expected aaudio or alsa)" >&2
        exit 1
        ;;
esac

case "$arch" in
    aarch64) target=aarch64-linux-android clang_target=aarch64-linux-android ;;
    x86_64) target=x86_64-linux-android clang_target=x86_64-linux-android ;;
    arm) target=armv7-linux-androideabi clang_target=armv7a-linux-androideabi ;;
    i686) target=i686-linux-android clang_target=i686-linux-android ;;
    *)
        echo "error: unknown arch '$arch' (expected aarch64, x86_64, arm or i686)" >&2
        exit 1
        ;;
esac

for command_name in cargo curl dpkg-deb pkg-config git python3; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "error: required command not found: $command_name" >&2
        exit 1
    fi
done
toolchain="$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin"
if [[ ! -x "$toolchain/${clang_target}${api}-clang" ]]; then
    echo "error: Android NDK not found at $ndk (set ANDROID_NDK_HOME)" >&2
    exit 1
fi

work_root="${CARGO_TARGET_DIR:-$project_dir/target}/termux/$audio"
work_dir="$work_root/$arch"
sysroot="$work_dir/sysroot"
source_dir="$work_dir/src"
mkdir -p "$work_dir/debs"
rm -rf "$sysroot" "$source_dir" "$work_dir/pkg"
mkdir -p "$sysroot" "$source_dir"

# Termux's own builds of the libraries nemuri links against, so the binary finds
# the same ones in $PREFIX/lib on the device.
packages_index="$work_dir/Packages"
curl -sSfL -o "$packages_index" "$repo_url/dists/stable/main/binary-$arch/Packages"
for package in "${link_packages[@]}"; do
    filename=$(awk -v want="Package: $package" '
        $0 == want { found = 1 }
        found && /^Filename: / { print $2; exit }
        /^$/ { found = 0 }
    ' "$packages_index")
    if [[ -z "$filename" ]]; then
        echo "error: $package not found in the Termux $arch repository" >&2
        exit 1
    fi
    deb="$work_dir/debs/$(basename "$filename")"
    [[ -f "$deb" ]] || curl -sSfL -o "$deb" "$repo_url/$filename"
    dpkg-deb -x "$deb" "$sysroot"
done

# Build from a copy of the tracked files, so the patched cpal and the lockfile
# change that comes with it stay out of the checkout.
git -C "$project_dir" ls-files -z | tar -C "$project_dir" --null -T - -cf - | tar -C "$source_dir" -xf -
cargo_args=(--target "$target")
if [[ "$audio" == alsa ]]; then
    cpal_manifest=$(cd "$source_dir" && cargo metadata --format-version 1 --locked --filter-platform "$target" |
        python3 -c 'import json, sys; print(next(p["manifest_path"] for p in json.load(sys.stdin)["packages"] if p["name"] == "cpal"))')
    cp -r "$(dirname "$cpal_manifest")" "$source_dir/vendor-cpal"
    find "$source_dir/vendor-cpal" -type f \( -name '*.rs' -o -name Cargo.toml \) -print0 |
        xargs -0 sed -i \
            -e 's|"android"|"disabling_this_because_it_is_for_building_an_apk"|g' \
            -e 's|"linux"|"android"|g'
    printf '\n[patch.crates-io]\ncpal = { path = "vendor-cpal" }\n' >> "$source_dir/Cargo.toml"
else
    cargo_args+=(--locked)
fi

target_env=${target//-/_}
target_env_upper=${target_env^^}
lib_dir="$sysroot$termux_prefix/lib"
export "CC_$target_env=$toolchain/${clang_target}${api}-clang"
export "AR_$target_env=$toolchain/llvm-ar"
export "CARGO_TARGET_${target_env_upper}_LINKER=$toolchain/${clang_target}${api}-clang"
export "CARGO_TARGET_${target_env_upper}_RUSTFLAGS=-L $lib_dir -C link-arg=-Wl,-rpath-link=$lib_dir -C link-arg=-Wl,-rpath=$termux_prefix/lib -C link-arg=-Wl,--enable-new-dtags"
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$lib_dir/pkgconfig"
export PKG_CONFIG_PATH=
export CARGO_TARGET_DIR="$work_dir/target"

if [[ "$mode" == tests ]]; then
    tests_dir="$work_dir/tests"
    rm -rf "$tests_dir"
    mkdir -p "$tests_dir"
    (cd "$source_dir" && cargo test --no-run --all-targets "${cargo_args[@]}" --message-format=json) |
        python3 -c '
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if message.get("reason") == "compiler-artifact" and message["profile"]["test"] and message["executable"]:
        print(message["executable"])
' |
        while read -r executable; do
            cp "$executable" "$tests_dir/"
        done
    echo "$tests_dir"
    exit 0
fi

(cd "$source_dir" && cargo build --release "${cargo_args[@]}")

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -n 1)
stage="$work_dir/pkg"
install -Dm755 "$CARGO_TARGET_DIR/$target/release/nemuri" "$stage$termux_prefix/bin/nemuri"
"$toolchain/llvm-strip" "$stage$termux_prefix/bin/nemuri"
install -Dm644 "$project_dir/packaging/nemuri.1" "$stage$termux_prefix/share/man/man1/nemuri.1"
gzip -9n "$stage$termux_prefix/share/man/man1/nemuri.1"
install -Dm644 -t "$stage$termux_prefix/share/doc/nemuri" "$project_dir/LICENSE" "$project_dir/README.md"
mkdir -p "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<EOF
Package: nemuri
Version: $version
Architecture: $arch
Maintainer: HZ-TYZQ
Installed-Size: $(du -sk "$stage$termux_prefix" | cut -f1)
Depends: $depends
Homepage: https://github.com/HZ-TYZQ/nemuri
Description: A terminal player for ASMR and music
EOF
output="$work_root/nemuri_${version}_$arch.deb"
dpkg-deb --root-owner-group -Zxz --build "$stage" "$output" >/dev/null
echo "$output"
