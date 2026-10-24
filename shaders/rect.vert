#version 450
// Instanced rounded rectangles with borders and drop shadows. The SDF and
// shadow math follow Warp's rect_shader.wgsl (warpui, MIT licensed).

layout(push_constant) uniform Push { vec2 viewport; } push;

layout(location = 0) in vec4 inBounds;      // x, y, width, height in pixels
layout(location = 1) in vec4 inColor;       // top color, straight alpha
layout(location = 2) in vec4 inColor2;      // bottom color (vertical gradient)
layout(location = 3) in vec4 inBorderColor;
layout(location = 4) in vec4 inRadii;       // top-left, top-right, bottom-right, bottom-left
layout(location = 5) in vec4 inParams;      // border width, shadow sigma
layout(location = 6) in vec4 inClip;        // x0, y0, x1, y1

layout(location = 0) out vec2 outPos;
layout(location = 1) flat out vec4 outBounds;
layout(location = 2) flat out vec4 outColor;
layout(location = 3) flat out vec4 outColor2;
layout(location = 4) flat out vec4 outBorderColor;
layout(location = 5) flat out vec4 outRadii;
layout(location = 6) flat out vec4 outParams;
layout(location = 7) flat out vec4 outClip;

void main() {
    vec2 corner = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    float sigma = inParams.y;
    vec2 origin = inBounds.xy;
    vec2 size = inBounds.zw;
    if (sigma > 0.0) {
        origin -= vec2(3.0 * sigma);
        size += vec2(6.0 * sigma);
    }
    vec2 pixel = origin + corner * size;
    gl_Position = vec4(pixel / push.viewport * 2.0 - 1.0, 0.0, 1.0);
    outPos = pixel;
    outBounds = inBounds;
    outColor = inColor;
    outColor2 = inColor2;
    outBorderColor = inBorderColor;
    outRadii = inRadii;
    outParams = inParams;
    outClip = inClip;
}
