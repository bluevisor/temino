#version 450

layout(push_constant) uniform Push { vec2 viewport; } push;

layout(location = 0) in vec4 inRect;   // x, y, width, height in pixels
layout(location = 1) in vec4 inUv;     // u0, v0, u1, v1
layout(location = 2) in vec4 inColor;  // straight alpha
layout(location = 3) in vec4 inClip;   // x0, y0, x1, y1

layout(location = 0) out vec2 outUv;
layout(location = 1) flat out vec4 outColor;
layout(location = 2) flat out vec4 outClip;

void main() {
    vec2 corner = vec2(gl_VertexIndex & 1, gl_VertexIndex >> 1);
    vec2 pixel = inRect.xy + corner * inRect.zw;
    gl_Position = vec4(pixel / push.viewport * 2.0 - 1.0, 0.0, 1.0);
    outUv = mix(inUv.xy, inUv.zw, corner);
    outColor = inColor;
    outClip = inClip;
}
