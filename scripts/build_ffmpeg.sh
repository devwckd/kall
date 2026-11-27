#!/usr/bin/env bash
# Build the small, static FFmpeg kall links on macOS and Linux (the `ffmpeg-static` feature):
# only the H.264 decoder and parser, the hardware H.264 encoder for the platform, and the
# scaler. Nothing to bundle with the installers, and no extra runtime libraries.
#
#   macOS: VideoToolbox (system framework).
#   Linux: NVENC (loads the NVIDIA driver at run time; nv-codec-headers are headers only), and
#          VAAPI for AMD and Intel (needs libva at build and run time).
# Plus GPU decoding (VideoToolbox / VAAPI), used with KALL_GPU_DECODE=1.
#
# usage: scripts/build_ffmpeg.sh <install prefix>
# Then: PKG_CONFIG_PATH=<prefix>/lib/pkgconfig cargo build -p kall --features ffmpeg-static
set -euo pipefail

PREFIX=$(mkdir -p "$1" && cd "$1" && pwd)
FFMPEG_TAG=n9.0.2
NVCODEC_TAG=n13.0.19.1
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cd "$WORK"

common=(
  --prefix="$PREFIX"
  --enable-static --disable-shared --enable-pic
  --disable-programs --disable-doc --disable-network --disable-autodetect
  --disable-everything
  --enable-avcodec --enable-avformat --enable-avutil --enable-swscale --enable-swresample
  --enable-decoder=h264 --enable-parser=h264
)

case "$(uname -s)" in
  Darwin)
    extra=(--enable-videotoolbox --enable-encoder=h264_videotoolbox --enable-hwaccel=h264_videotoolbox)
    ;;
  Linux)
    curl -sSL "https://github.com/FFmpeg/nv-codec-headers/archive/refs/tags/$NVCODEC_TAG.tar.gz" | tar xz
    make -C "nv-codec-headers-$NVCODEC_TAG" PREFIX="$PREFIX" install > /dev/null
    export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
    extra=(--enable-ffnvcodec --enable-nvenc --enable-encoder=h264_nvenc
           --enable-vaapi --enable-encoder=h264_vaapi --enable-hwaccel=h264_vaapi)
    ;;
  *)
    echo "build_ffmpeg.sh is for macOS and Linux (Windows uses BtbN's shared build)" >&2
    exit 1
    ;;
esac

curl -sSL "https://github.com/FFmpeg/FFmpeg/archive/refs/tags/$FFMPEG_TAG.tar.gz" | tar xz
cd "FFmpeg-$FFMPEG_TAG"
./configure "${common[@]}" "${extra[@]}" > configure.log || { tail -40 configure.log; exit 1; }
make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || sysctl -n hw.ncpu)" > /dev/null
make install > /dev/null
echo "FFmpeg $FFMPEG_TAG installed in $PREFIX"
