// The GLSL shaders in this directory, in the Metal Shading Language, for
// the macOS renderer (src/macos/shim.m). Metal's clip space has y up, so
// the vertex stages flip it.

#include <metal_stdlib>
using namespace metal;

struct RectInstance {
    float4 bounds;      // x, y, width, height in pixels
    float4 color;       // top color, straight alpha
    float4 color2;      // bottom color (vertical gradient)
    float4 borderColor;
    float4 radii;       // top-left, top-right, bottom-right, bottom-left
    float4 params;      // border width, shadow sigma
    float4 clip;        // x0, y0, x1, y1
};

struct GlyphInstance {
    float4 rect;
    float4 uv;
    float4 color;
    float4 clip;
};

struct RectOut {
    float4 position [[position]];
    float2 pos;
    float4 bounds [[flat]];
    float4 color [[flat]];
    float4 color2 [[flat]];
    float4 borderColor [[flat]];
    float4 radii [[flat]];
    float4 params [[flat]];
    float4 clip [[flat]];
};

struct GlyphOut {
    float4 position [[position]];
    float2 uv;
    float4 color [[flat]];
    float4 clip [[flat]];
};

static float4 toClip(float2 pixel, float2 viewport) {
    float2 ndc = pixel / viewport * 2.0 - 1.0;
    return float4(ndc.x, -ndc.y, 0.0, 1.0);
}

vertex RectOut rectVertex(uint vid [[vertex_id]], uint iid [[instance_id]],
                          const device RectInstance* instances [[buffer(0)]],
                          constant float2& viewport [[buffer(1)]]) {
    RectInstance r = instances[iid];
    float2 corner = float2(vid & 1, vid >> 1);
    float sigma = r.params.y;
    float2 origin = r.bounds.xy;
    float2 size = r.bounds.zw;
    if (sigma > 0.0) {
        origin -= float2(3.0 * sigma);
        size += float2(6.0 * sigma);
    }
    float2 pixel = origin + corner * size;
    RectOut out;
    out.position = toClip(pixel, viewport);
    out.pos = pixel;
    out.bounds = r.bounds;
    out.color = r.color;
    out.color2 = r.color2;
    out.borderColor = r.borderColor;
    out.radii = r.radii;
    out.params = r.params;
    out.clip = r.clip;
    return out;
}

static float roundedBoxDistance(float2 p, float2 halfSize, float radius) {
    float2 q = abs(p) - halfSize + radius;
    return length(max(q, float2(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

static float2 erf2(float2 x) {
    float2 s = sign(x);
    float2 a = abs(x);
    float2 d = 1.0 + (0.278393 + (0.230389 + 0.078108 * (a * a)) * a) * a;
    d *= d;
    return s - s / (d * d);
}

static float gaussian(float x, float sigma) {
    return exp(-(x * x) / (2.0 * sigma * sigma)) / (sqrt(2.0 * M_PI_F) * sigma);
}

static float shadowX(float x, float y, float sigma, float corner, float2 halfSize) {
    float delta = min(halfSize.y - corner - abs(y), 0.0);
    float curved = halfSize.x - corner + sqrt(max(0.0, corner * corner - delta * delta));
    float2 integral = 0.5 + 0.5 * erf2((x + float2(-curved, curved)) * (sqrt(0.5) / sigma));
    return integral.y - integral.x;
}

static float roundedBoxShadow(float2 halfSize, float2 point, float sigma, float corner) {
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

fragment float4 rectFragment(RectOut in [[stage_in]]) {
    float2 frag = in.position.xy;
    if (frag.x < in.clip.x || frag.y < in.clip.y || frag.x >= in.clip.z || frag.y >= in.clip.w)
        discard_fragment();

    float2 halfSize = in.bounds.zw * 0.5;
    float2 p = in.pos - (in.bounds.xy + halfSize);
    float corner = p.y < 0.0 ? (p.x < 0.0 ? in.radii.x : in.radii.y) : (p.x < 0.0 ? in.radii.w : in.radii.z);
    float radius = min(corner, min(halfSize.x, halfSize.y));
    float sigma = in.params.y;

    if (sigma > 0.0) {
        float a = in.color.a * roundedBoxShadow(halfSize, p, sigma, radius);
        return float4(in.color.rgb * a, a);
    }

    float t = clamp((in.pos.y - in.bounds.y) / max(in.bounds.w, 1.0), 0.0, 1.0);
    float4 fill = mix(in.color, in.color2, t);

    float outer = roundedBoxDistance(p, halfSize, radius);
    float coverage = clamp(0.5 - outer, 0.0, 1.0);

    float border = in.params.x;
    float4 color = fill;
    if (border > 0.0) {
        float inner = roundedBoxDistance(p, halfSize - float2(border), max(radius - border, 0.0));
        float edge = clamp(0.5 + inner, 0.0, 1.0);
        float4 b = in.borderColor;
        float a = b.a * edge + fill.a * (1.0 - b.a * edge);
        float3 rgb = (b.rgb * b.a * edge + fill.rgb * fill.a * (1.0 - b.a * edge)) / max(a, 1e-6);
        color = float4(rgb, a);
    }
    float alpha = color.a * coverage;
    return float4(color.rgb * alpha, alpha);
}

vertex GlyphOut glyphVertex(uint vid [[vertex_id]], uint iid [[instance_id]],
                            const device GlyphInstance* instances [[buffer(0)]],
                            constant float2& viewport [[buffer(1)]]) {
    GlyphInstance g = instances[iid];
    float2 corner = float2(vid & 1, vid >> 1);
    GlyphOut out;
    out.position = toClip(g.rect.xy + corner * g.rect.zw, viewport);
    out.uv = mix(g.uv.xy, g.uv.zw, corner);
    out.color = g.color;
    out.clip = g.clip;
    return out;
}

fragment float4 glyphFragment(GlyphOut in [[stage_in]], texture2d<float> atlas [[texture(0)]]) {
    float2 frag = in.position.xy;
    if (frag.x < in.clip.x || frag.y < in.clip.y || frag.x >= in.clip.z || frag.y >= in.clip.w)
        discard_fragment();
    constexpr sampler nearest(filter::nearest, address::clamp_to_edge);
    // A slight gamma lift: grayscale coverage blended in sRGB space reads thin.
    float coverage = pow(atlas.sample(nearest, in.uv).r, 0.8);
    float alpha = in.color.a * coverage;
    return float4(in.color.rgb * alpha, alpha);
}
