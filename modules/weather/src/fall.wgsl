// What falls around the eye (after the fog and light WGSL of the view): rain, snow or sand,
// each particle a quad made by the vertex shader from its instance alone, its place in a box around
// the eye computed from its number and the time, so that the CPU writes one uniform a frame
// whatever their count. The particles stay where they are in the world as the eye moves, the box
// wrapping them around it, and fade towards its sides.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour and the light everywhere, in gamma.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    // The eye, and the time in seconds.
    eye: vec4<f32>,
    // The colour of the fog, in gamma; where it starts, its middle and its end, and the rate of the
    // game's fog, 0 for the editor's.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
    // The axes of the camera, across and up.
    across: vec4<f32>,
    up: vec4<f32>,
    // The colour of a particle in gamma, and its alpha.
    colour: vec4<f32>,
    // Its width and its length in yards, whether it is a streak along its fall (1) or a flake
    // facing the eye (0), and how far it sways.
    shape: vec4<f32>,
    // The wind across X and Y and the speed of the fall, in yards a second, and the height under
    // which nothing is drawn: the surface of the water under the eye.
    motion: vec4<f32>,
    // Half the side of the box around the eye, its height, in yards.
    space: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

struct Out {
    @builtin(position) clip: vec4<f32>,
    // Where on its quad, across from -1 to 1 and along from -1 to 1.
    @location(0) quad: vec2<f32>,
    @location(1) world: vec3<f32>,
    // How much it shows, faded towards the sides of the box.
    @location(2) shown: f32,
};

// A number of `n` spread over all 32 bits (PCG).
fn hash(n: u32) -> u32 {
    let state = n * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// A number from 0 to 1 of `n`.
fn unit(n: u32) -> f32 {
    return f32(hash(n) >> 8u) / 16777216.0;
}

@vertex
fn vs_main(@builtin(vertex_index) corner: u32, @builtin(instance_index) index: u32) -> Out {
    let time = camera.eye.w;
    let half = camera.space.x;
    let size = vec3<f32>(2.0 * half, 2.0 * half, camera.space.y);
    let seed = vec3<f32>(unit(index * 4u), unit(index * 4u + 1u), unit(index * 4u + 2u));
    // Its place in the world: each falls a little faster or slower than the others, carried by the
    // wind, swaying as it goes.
    let speed = camera.motion.z * (0.8 + 0.4 * unit(index * 4u + 3u));
    let phase = seed.xy * 6.2831853;
    let sway = camera.shape.w * vec2<f32>(sin(time * 1.3 + phase.x), cos(time * 1.1 + phase.y));
    let fall = vec3<f32>(camera.motion.xy, -speed);
    let moved = seed * size + vec3<f32>(camera.motion.xy * time + sway, -speed * time);
    // Within the box around the eye, wrapped: its place relative to the eye from -0.5 to 0.5 of the
    // box on each axis.
    let local = fract((moved - camera.eye.xyz) / size + vec3<f32>(0.5)) - vec3<f32>(0.5);
    let centre = camera.eye.xyz + local * size;
    // The two triangles of its quad: across, then along.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, 1.0), vec2<f32>(-1.0, 1.0),
    );
    let quad = corners[corner % 6u];
    var world: vec3<f32>;
    if camera.shape.z > 0.5 {
        // A streak along its fall, turned to face the eye as far as it can.
        let along = normalize(fall);
        var side = cross(along, centre - camera.eye.xyz);
        if dot(side, side) < 1e-8 {
            side = camera.across.xyz;
        }
        world = centre + normalize(side) * quad.x * camera.shape.x * 0.5 + along * quad.y * camera.shape.y * 0.5;
    } else {
        world = centre + (camera.across.xyz * quad.x + camera.up.xyz * quad.y) * camera.shape.x * 0.5;
    }
    var out: Out;
    out.quad = quad;
    out.world = world;
    // Fading out towards the sides of the box, so that none appears nor leaves at once.
    let reach = max(length(local.xy) * 2.0, abs(local.z) * 2.0);
    out.shown = 1.0 - smoothstep(0.7, 1.0, reach);
    // Nothing under the surface of the water.
    if centre.z < camera.motion.w {
        out.clip = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    } else {
        out.clip = camera.view_proj * vec4<f32>(world, 1.0);
    }
    return out;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    // A streak soft across and at its ends, a flake round.
    var shape: f32;
    if camera.shape.z > 0.5 {
        shape = (1.0 - abs(in.quad.x)) * (1.0 - in.quad.y * in.quad.y);
    } else {
        shape = clamp(1.0 - length(in.quad), 0.0, 1.0);
    }
    let alpha = camera.colour.a * shape * in.shown;
    if alpha <= 1.0 / 255.0 {
        discard;
    }
    // Lit by the light of the view from above, then fogged as the view fogs.
    let lit = camera.colour.rgb * light(vec3<f32>(0.0, 0.0, 1.0));
    return vec4<f32>(fog_mix(lit, camera.fog_colour.rgb, fog_amount(in.world)), alpha);
}
