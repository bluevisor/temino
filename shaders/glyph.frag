#version 450

layout(set = 0, binding = 0) uniform sampler2D atlas;

layout(location = 0) in vec2 inUv;
layout(location = 1) flat in vec4 inColor;
layout(location = 2) flat in vec4 inClip;

layout(location = 0) out vec4 outColor;

void main() {
    vec2 frag = gl_FragCoord.xy;
    if (frag.x < inClip.x || frag.y < inClip.y || frag.x >= inClip.z || frag.y >= inClip.w)
        discard;
    // A slight gamma lift: grayscale coverage blended in sRGB space reads thin.
    float coverage = pow(texture(atlas, inUv).r, 0.8);
    float alpha = inColor.a * coverage;
    outColor = vec4(inColor.rgb * alpha, alpha);
}
