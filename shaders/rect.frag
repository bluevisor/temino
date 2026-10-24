#version 450

layout(location = 0) in vec2 inPos;
layout(location = 1) flat in vec4 inBounds;
layout(location = 2) flat in vec4 inColor;
layout(location = 3) flat in vec4 inColor2;
layout(location = 4) flat in vec4 inBorderColor;
layout(location = 5) flat in vec4 inRadii;
layout(location = 6) flat in vec4 inParams;
layout(location = 7) flat in vec4 inClip;

layout(location = 0) out vec4 outColor;

const float PI = 3.141592653589793;

float roundedBoxDistance(vec2 p, vec2 halfSize, float radius) {
    vec2 q = abs(p) - halfSize + radius;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

float cornerRadius(vec2 p) {
    // p is relative to the center; radii are tl, tr, br, bl.
    if (p.y < 0.0)
        return p.x < 0.0 ? inRadii.x : inRadii.y;
    return p.x < 0.0 ? inRadii.w : inRadii.z;
}

// Fast rounded-rectangle shadows (madebyevan.com), as in Warp's shader.
vec2 erf2(vec2 x) {
    vec2 s = sign(x);
    vec2 a = abs(x);
    vec2 d = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    d *= d;
    return s - s / (d * d);
}

float gaussian(float x, float sigma) {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (sqrt(2.0 * PI) * sigma);
}

float shadowX(float x, float y, float sigma, float corner, vec2 halfSize) {
    float delta = min(halfSize.y - corner - abs(y), 0.0);
    float curved = halfSize.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    vec2 integral = 0.5 + 0.5 * erf2((x + vec2(-curved, curved)) * (sqrt(0.5) / sigma));
    return integral.y - integral.x;
}

float roundedBoxShadow(vec2 halfSize, vec2 point, float sigma, float corner) {
    float low = point.y - halfSize.y;
    float high = point.y + halfSize.y;
    float start = clamp(-3.0 * sigma, low, high);
    float end = clamp(3.0 * sigma, low, high);
    float stepSize = (end - start) / 4.0;
    float y = start + stepSize * 0.5;
    float value = 0.0;
    for (int i = 0; i < 4; i++) {
        value += shadowX(point.x, point.y - y, sigma, corner, halfSize) * gaussian(y, sigma) * stepSize;
        y += stepSize;
    }
    return value;
}

void main() {
    vec2 frag = gl_FragCoord.xy;
    if (frag.x < inClip.x || frag.y < inClip.y || frag.x >= inClip.z || frag.y >= inClip.w)
        discard;

    vec2 halfSize = inBounds.zw * 0.5;
    vec2 p = inPos - (inBounds.xy + halfSize);
    float radius = min(cornerRadius(p), min(halfSize.x, halfSize.y));
    float sigma = inParams.y;

    if (sigma > 0.0) {
        float a = inColor.a * roundedBoxShadow(halfSize, p, sigma, radius);
        outColor = vec4(inColor.rgb * a, a);
        return;
    }

    float t = clamp((inPos.y - inBounds.y) / max(inBounds.w, 1.0), 0.0, 1.0);
    vec4 fill = mix(inColor, inColor2, t);

    float outer = roundedBoxDistance(p, halfSize, radius);
    float coverage = clamp(0.5 - outer, 0.0, 1.0);

    float border = inParams.x;
    vec4 color = fill;
    if (border > 0.0) {
        float inner = roundedBoxDistance(p, halfSize - vec2(border), max(radius - border, 0.0));
        float edge = clamp(0.5 + inner, 0.0, 1.0);
        // "over" composite of the border on the fill
        float a = inBorderColor.a * edge + fill.a * (1.0 - inBorderColor.a * edge);
        vec3 rgb = (inBorderColor.rgb * inBorderColor.a * edge
            + fill.rgb * fill.a * (1.0 - inBorderColor.a * edge)) / max(a, 1e-6);
        color = vec4(rgb, a);
    }
    float alpha = color.a * coverage;
    outColor = vec4(color.rgb * alpha, alpha);
}
