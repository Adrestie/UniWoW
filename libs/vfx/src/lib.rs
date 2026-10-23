//! The spell effects of the 3.3.5a client, made here from their description: the description
//! itself (`json`, the files of WoW-mods' `tools/vfx`, read and written the same), the textures
//! (`texgen`, `blp`), the model and its skin (`forge`, `m2`) and the checks the client's own spell
//! models pass (`validate`). Nothing here goes past what the client reads: M2 264, its skin, BLP2.

pub mod anim;
pub mod blp;
pub mod forge;
pub mod frame;
pub mod gltf;
pub mod json;
pub mod m2;
pub mod py;
pub mod sim;
pub mod texgen;
pub mod validate;

#[cfg(test)]
mod tests;
