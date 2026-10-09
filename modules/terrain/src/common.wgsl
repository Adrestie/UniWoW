// What the shaders of the terrain share: the camera, the sun and the fog of the view.

struct Camera {
    view_proj: mat4x4<f32>,
    // The direction towards the sun, its colour on the ground and the light everywhere, in gamma.
    sun: vec4<f32>,
    sun_colour: vec4<f32>,
    ambient: vec4<f32>,
    eye: vec4<f32>,
    // The colour of the fog, and of the sky without a light; where it starts, its middle and where it
    // covers all, and the rate of the game's fog, 0 for the editor's.
    fog_colour: vec4<f32>,
    fog: vec4<f32>,
    // From a place on the screen to the direction it is seen in from the eye; the colours of the
    // sky of the light, in gamma, from its top down to its fog, their alpha 0 without it.
    sky_from_screen: mat4x4<f32>,
    sky: array<vec4<f32>, 6>,
};
@group(0) @binding(0) var<uniform> camera: Camera;

// Mixed with the fog of the view at `position`.
fn fogged(colour: vec3<f32>, position: vec3<f32>) -> vec3<f32> {
    return fog_mix(colour, camera.fog_colour.rgb, fog_amount(position));
}
