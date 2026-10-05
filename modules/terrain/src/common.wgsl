// What the shaders of the terrain share: the camera, the sun and the fog of the view.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour, and the light everywhere.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    eye: vec4<f32>,
    // The colour of the fog and of the sky; where it starts, its middle and where it covers all,
    // on the ground.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// The share of the fog at its middle.
const NEAR_FOG: f32 = 0.55;

// The fog by the distance on the ground, as the client hides its limits.
fn fog_amount(position: vec3<f32>) -> f32 {
    let distance = length(position.xy - camera.eye.xy);
    return NEAR_FOG * smoothstep(camera.fog.x, camera.fog.y, distance)
        + (1.0 - NEAR_FOG) * smoothstep(camera.fog.y, camera.fog.z, distance);
}

fn fogged(colour: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    return mix(colour, camera.fog_colour.rgb, fog_amount(position));
}

fn light(normal: vec3<f32>) -> vec3<f32> {
    return camera.ambient.rgb + camera.sun_colour.rgb * max(dot(normalize(normal), camera.sun.xyz), 0.0);
}
