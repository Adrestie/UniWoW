// The models: a batch of a model for every instance of a group, its texture modulated by its colour
// and transparency at rest and by the instance's alpha, lit by the sun and fogged as the view says.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour, and the light everywhere.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    // The eye, and how far an instance is drawn: this many times its radius.
    eye: vec4<f32>,
    // The colour of the fog; where it starts, its middle and where it covers all, on the ground.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct Batch {
    // Its colour and transparency at rest, its weight included.
    colour: vec4<f32>,
    // Its alpha key, the share of its alpha under which a pixel is not drawn, 0 for none; 1 when
    // unlit; 1 when unfogged; the colour of its fog: 0 the view's, 1 black, 2 white, 3 grey.
    flags: vec4<f32>,
    // The radius of the model; 1 when the alpha of its texture counts, 0 when opaque.
    model: vec4<f32>,
};
@group(1) @binding(0) var<uniform> batch: Batch;
@group(1) @binding(1) var diffuse: texture_2d<f32>;
@group(1) @binding(2) var diffuse_sampler: sampler;

// The share of the fog at its middle; the least radius an instance's reach counts; the alpha under
// which a pixel of a batch without an alpha key is not drawn.
const NEAR_FOG: f32 = 0.55;
const LEAST_RADIUS: f32 = 1.0;
const LEAST_ALPHA: f32 = 1.0 / 255.0;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    // The instance: the rows of its transform, then its alpha.
    @location(3) row0: vec4<f32>,
    @location(4) row1: vec4<f32>,
    @location(5) row2: vec4<f32>,
    @location(6) extra: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) world: vec3<f32>,
    @location(3) alpha: f32,
};

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    let position = vec4<f32>(in.position, 1.0);
    let world = vec3<f32>(dot(in.row0, position), dot(in.row1, position), dot(in.row2, position));
    let origin = vec3<f32>(in.row0.w, in.row1.w, in.row2.w);
    let scale = length(vec3<f32>(in.row0.x, in.row1.x, in.row2.x));
    let reach = camera.eye.w * max(batch.model.x * scale, LEAST_RADIUS);
    var out: VertexOut;
    if distance(origin, camera.eye.xyz) > reach || in.extra.x <= 0.0 {
        // Beyond its reach, or unseen: every vertex at one point, its triangles draw nothing.
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    } else {
        out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    }
    let normal = vec4<f32>(in.normal, 0.0);
    out.normal = vec3<f32>(dot(in.row0, normal), dot(in.row1, normal), dot(in.row2, normal));
    out.uv = in.uv;
    out.world = world;
    out.alpha = in.extra.x;
    return out;
}

fn fog_amount(position: vec3<f32>) -> f32 {
    let distance = length(position.xy - camera.eye.xy);
    return NEAR_FOG * smoothstep(camera.fog.x, camera.fog.y, distance)
        + (1.0 - NEAR_FOG) * smoothstep(camera.fog.y, camera.fog.z, distance);
}

fn fog_colour(mode: f32) -> vec3<f32> {
    if mode > 2.5 {
        return vec3<f32>(0.5);
    }
    if mode > 1.5 {
        return vec3<f32>(1.0);
    }
    if mode > 0.5 {
        return vec3<f32>(0.0);
    }
    return camera.fog_colour.rgb;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let texel = textureSample(diffuse, diffuse_sampler, in.uv);
    // As WotLK tests it: the alpha of the batch and the instance, times the texel's but when
    // opaque, against the alpha key times the alpha of the batch, or 1/255.
    let element = batch.colour.a * in.alpha;
    let alpha = element * mix(1.0, texel.a, batch.model.y);
    let reference = select(LEAST_ALPHA, batch.flags.x * element, batch.flags.x > 0.0);
    if alpha < reference {
        discard;
    }
    var rgb = texel.rgb * batch.colour.rgb;
    if batch.flags.y < 0.5 {
        let lit = max(dot(normalize(in.normal), camera.sun.xyz), 0.0);
        rgb = rgb * (camera.ambient.rgb + camera.sun_colour.rgb * lit);
    }
    if batch.flags.z < 0.5 {
        rgb = mix(rgb, fog_colour(batch.flags.w), fog_amount(in.world));
    }
    return vec4<f32>(rgb, alpha);
}
