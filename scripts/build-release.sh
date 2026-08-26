#!/usr/bin/env bash
set -euo pipefail
umask 022

project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$project_root/Cargo.toml" | head -n 1)"
package_name="WinSched-${version}-windows-x64"
dist_dir="$project_root/dist"
stage_dir="$dist_dir/$package_name"
resource_compiler="${RC_PATH:-/usr/lib/llvm-18/bin/llvm-rc}"

if [[ ! -x "$resource_compiler" ]]; then
    echo "LLVM resource compiler not found: $resource_compiler" >&2
    exit 1
fi

cd "$project_root"
cargo fmt --all -- --check
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo audit
RC_PATH="$resource_compiler" cargo xwin clippy --workspace --all-targets \
    --target x86_64-pc-windows-msvc -- -D warnings
RC_PATH="$resource_compiler" cargo xwin build --workspace --release \
    --target x86_64-pc-windows-msvc
if objdump -p target/x86_64-pc-windows-msvc/release/winsched-tray.exe | \
    grep TaskDialogIndirect >/dev/null; then
    echo "winsched-tray.exe unexpectedly imports TaskDialogIndirect" >&2
    exit 1
fi

mkdir -p "$dist_dir"
if [[ -e "$stage_dir" ]]; then
    rm -rf -- "$stage_dir"
fi
mkdir -p "$stage_dir"

target_dir="$project_root/target/x86_64-pc-windows-msvc/release"
cp "$target_dir/winsched.exe" "$stage_dir/"
cp "$target_dir/winsched-service.exe" "$stage_dir/"
cp "$target_dir/winsched-tray.exe" "$stage_dir/"
cp "$target_dir/winsched-settings.exe" "$stage_dir/"
cp "$project_root/config/winsched.default.toml" "$stage_dir/winsched.toml"
cp "$project_root/installer/secure-data.ps1" "$stage_dir/"
cp "$project_root/README.md" "$stage_dir/README.md"
cp "$project_root/LICENSE" "$stage_dir/LICENSE"

(
    cd "$stage_dir"
    sha256sum winsched.exe winsched-service.exe winsched-tray.exe \
        winsched-settings.exe winsched.toml secure-data.ps1 README.md LICENSE > SHA256SUMS
)

archive="$dist_dir/$package_name.zip"
rm -f -- "$archive"
(
    cd "$dist_dir"
    zip -qr "$archive" "$package_name"
)
(
    cd "$dist_dir"
    sha256sum "$package_name.zip" > "$package_name.zip.sha256"
)
echo "release package: $archive"
