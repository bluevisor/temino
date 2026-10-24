#!/bin/sh
# Compiles the GLSL shaders to the SPIR-V that src/gpu.rs embeds.
# Needs glslc (brew install shaderc); run it after editing a shader.
set -e
cd "$(dirname "$0")"
for shader in rect.vert rect.frag glyph.vert glyph.frag; do
	glslc -O --target-env=vulkan1.1 -o "$shader.spv" "$shader"
done
spirv-val --target-env vulkan1.1 rect.vert.spv rect.frag.spv glyph.vert.spv glyph.frag.spv 2>/dev/null || true
