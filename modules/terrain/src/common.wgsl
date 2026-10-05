// What the shaders of the terrain share: the camera and the fog, until the lights of the map come.

struct Camera {
    view_proj: mat4x4<f32>,
    sun: vec4<f32>,
    // The eye, and the reach of the tiles loaded around it, on the ground.
    eye: vec4<f32>,
    // The colour of the fog and of the sky, and the farthest of the map from the eye, on the ground.
    fog: vec4<f32>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// The fog by the distance on the ground, as the client hides its limits: half of it at the reach
// of the tiles loaded, where the horizon starts, all of it at the farthest of the map.
fn fog_amount(position: vec3<f32>) -> f32 {
    let distance = length(position.xy - camera.eye.xy);
    let reach = camera.eye.w;
    return 0.55 * smoothstep(0.5 * reach, reach, distance)
        + 0.45 * smoothstep(reach, max(camera.fog.w, reach * 1.01), distance);
}

fn fogged(colour: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    return mix(colour, camera.fog.rgb, fog_amount(position));
}

fn light(normal: vec3<f32>) -> f32 {
    return 0.45 + 0.55 * max(dot(normalize(normal), camera.sun.xyz), 0.0);
}
