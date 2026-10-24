#!/usr/bin/env bash
# Builds what a release of breeze-mock is:
#
#   packaging/out/breeze-mock-VERSION-1.x86_64.rpm     static musl, linked by zig
#   packaging/out/breeze-mock-VERSION-1.aarch64.rpm
#   packaging/out/breeze-mock-VERSION-windows-x86_64.zip   (on Windows only)
#   packaging/out/SHA256SUMS
#
# Needs cargo-zigbuild, zig 0.16.0 (BM_ZIG, else `zig` on PATH; another
# version is refused, as Breeze Core's own build refuses it) and Docker, for
# nfpm. Nothing is bind-mounted: the staged tree goes into the packaging
# container on stdin and the packages come back the same way, so this works
# from a drive Docker does not share.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
VERSION="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
OUT="packaging/out"
STAGE="$OUT/stage"
IMAGE="breeze-mock-nfpm:2.47.0"

ZIG="${BM_ZIG:-zig}"
zig_have="$("$ZIG" version 2>/dev/null || echo none)"
if [ "$zig_have" != "0.16.0" ]; then
  echo "!! zig $zig_have at '$ZIG'; this build is pinned to 0.16.0."
  echo "   Run with BM_ZIG=/path/to/zig-0.16.0/zig"
  exit 1
fi
export CARGO_ZIGBUILD_ZIG_PATH="$ZIG"

for f in README.md docs/blueprint.md LICENSE schema/blueprint.xsd; do
  [ -f "$f" ] || { echo "!! $f is missing"; exit 1; }
done

rm -rf "$STAGE"
mkdir -p "$STAGE/share/doc" "$STAGE/share/examples" "$STAGE/share/schema" "$STAGE/scripts"

# label (nfpm's arch) | rust target
for pair in amd64:x86_64-unknown-linux-musl arm64:aarch64-unknown-linux-musl; do
  label="${pair%%:*}" target="${pair#*:}"
  echo "== $target"
  cargo zigbuild --release --locked --target "$target"
  mkdir -p "$STAGE/bin/$label"
  cp "target/$target/release/breeze-mock" "$STAGE/bin/$label/"
done

cp packaging/nfpm.yaml packaging/breeze-mock.service packaging/blueprint.xml "$STAGE/"
cp packaging/scripts/*.sh "$STAGE/scripts/"
cp schema/blueprint.xsd "$STAGE/share/schema/"
cp examples/*.xml "$STAGE/share/examples/"
cp README.md docs/*.md "$STAGE/share/doc/"
cp LICENSE "$STAGE/share/"

echo "== packaging image"
docker build -q -t "$IMAGE" packaging >/dev/null
echo "== rpm"
rm -f "$OUT"/*.rpm
tar -C "$STAGE" -cf - . | docker run --rm -i -e VERSION="$VERSION" "$IMAGE" bash -c '
  set -euo pipefail
  tar -xf - -C /work
  for arch in amd64 arm64; do
    mkdir -p current && cp "bin/$arch/breeze-mock" current/
    ARCH=$arch nfpm package -f nfpm.yaml -p rpm -t /tmp/ >&2
  done
  for p in /tmp/*.rpm; do rpm -qpi "$p" >&2; rpm -qplv "$p" >&2; done
  tar -C /tmp -cf - $(cd /tmp && ls *.rpm)
' | tar -xf - -C "$OUT"

# Windows: the host build, zipped with what the packages carry.
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*)
    echo "== windows zip"
    cargo build --release --locked
    z="breeze-mock-$VERSION-windows-x86_64"
    rm -rf "$OUT/$z" "$OUT/$z.zip"
    mkdir -p "$OUT/$z/schema" "$OUT/$z/examples" "$OUT/$z/docs"
    cp target/release/breeze-mock.exe README.md LICENSE "$OUT/$z/"
    cp schema/blueprint.xsd "$OUT/$z/schema/"
    cp examples/*.xml "$OUT/$z/examples/"
    cp docs/*.md "$OUT/$z/docs/"
    # Windows' own bsdtar: Compress-Archive in Windows PowerShell writes
    # backslashes into the names, which unzip elsewhere does not take.
    (cd "$OUT" && /c/Windows/System32/tar.exe -a -cf "$z.zip" "$z")
    rm -rf "$OUT/$z"
    ;;
esac

(cd "$OUT" && shopt -s nullglob && sha256sum -- *.rpm *.zip | sed 's/ \*/  /' > SHA256SUMS)
echo "== done"
ls -l "$OUT"
cat "$OUT/SHA256SUMS"
