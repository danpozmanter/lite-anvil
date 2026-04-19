#!/usr/bin/env bash
#
# Build SDL3 without OpenGL/Vulkan/GPU support.
#
# This produces a lightweight SDL3 (~3.4MB) that uses pure software
# rendering via X11 shared-memory surfaces, avoiding the GPU driver
# overhead (~70MB RSS on NVIDIA).
#
# Prerequisites:
#   - SDL3 source tree (git clone https://github.com/libsdl-org/SDL)
#   - cmake, gcc/clang, X11 dev headers
#
# Usage:
#   ./scripts/build-sdl3-nogl.sh /path/to/SDL-source
#
# Output is installed to lib/sdl3-nogl/ in the project root.

set -euo pipefail

SDL_SRC="${1:?Usage: $0 /path/to/SDL-source}"
PROJECT_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_DIR="${SDL_SRC}/build-nogl"
INSTALL_DIR="${PROJECT_ROOT}/lib/sdl3-nogl"

echo "SDL source:  ${SDL_SRC}"
echo "Build dir:   ${BUILD_DIR}"
echo "Install dir: ${INSTALL_DIR}"

mkdir -p "${BUILD_DIR}"
cd "${BUILD_DIR}"

cmake "${SDL_SRC}" \
    -DSDL_OPENGL=OFF \
    -DSDL_OPENGLES=OFF \
    -DSDL_VULKAN=OFF \
    -DSDL_GPU=OFF \
    -DSDL_RENDER=OFF \
    -DSDL_X11=ON \
    -DSDL_WAYLAND=OFF \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="${INSTALL_DIR}"

make -j"$(nproc)"

# Install only the shared library, not headers or cmake config.
mkdir -p "${INSTALL_DIR}"
cp -P libSDL3.so* "${INSTALL_DIR}/"

echo ""
echo "No-GL SDL3 installed to ${INSTALL_DIR}:"
ls -la "${INSTALL_DIR}"/libSDL3*
echo ""
echo "Nano Anvil will link against this automatically via nano-anvil/build.rs."
