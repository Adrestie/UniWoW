//! UniWoW kernel: loads the module DLLs and provides the core services.
//!
//! The kernel is linked into the executable. Modules never depend on it: they only see the
//! contracts of `uniwow-api`.

mod capi;
mod compiled;
mod draw;
mod gpu_memory;
mod groups;
mod guard;
mod history;
mod host;
mod hotkeys;
mod jobs;
mod layout;
mod loader;
mod logger;
mod manifest;
mod order;
mod panels;
mod players;
#[cfg(test)]
mod random;
mod requirements;
mod router;
mod settings;
mod shell;

use std::sync::Arc;

use uniwow_api::{eframe, egui, egui_wgpu, wgpu};

pub fn run() -> eframe::Result {
    logger::install();
    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(setup) = &mut wgpu_options.wgpu_setup {
        setup.device_descriptor = with_features(setup.device_descriptor.clone());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("UniWoW")
            .with_inner_size([1400.0, 900.0]),
        wgpu_options,
        ..Default::default()
    };
    eframe::run_native("UniWoW", options, Box::new(|cc| Ok(Box::new(shell::Shell::new(cc)))))
}

/// How egui-wgpu asks for the device.
type DeviceDescriptor = Arc<dyn Fn(&wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> + Send + Sync>;

/// What the device asks for beyond egui-wgpu, when the adapter offers it: the block compressions
/// of the textures of WoW (BC1 to BC3), which then go to the GPU as they are stored; the
/// timestamps the statistics of the view time the GPU with, inside encoders and passes for each
/// layer apart; the first instance of an indirect draw, and the count of a multi-draw read from a
/// buffer, for the layers whose draws the GPU chooses.
const WANTED: wgpu::Features = wgpu::Features::TEXTURE_COMPRESSION_BC
    .union(wgpu::Features::TIMESTAMP_QUERY)
    .union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
    .union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES)
    .union(wgpu::Features::INDIRECT_FIRST_INSTANCE)
    .union(wgpu::Features::MULTI_DRAW_INDIRECT_COUNT);

/// The sampled textures a stage of a shader may bind, at most this many where the adapter takes
/// them: the models read their arrays of textures by 64 slots.
const SAMPLED_TEXTURES: u32 = 128;

/// The blocks of memory of the device its allocator places the buffers and textures in, from the
/// first to the largest: with the 256 MB of `MemoryHints::Performance`, a few small buffers held
/// long kept gigabytes reserved.
const MEMORY_BLOCKS: std::ops::Range<u64> = (32 << 20)..(128 << 20);

/// The device as `default` asks for it, with what `WANTED` names when the adapter offers it; as
/// many layers in an array of textures as the adapter takes (the terrain keeps its textures in
/// arrays, 256 layers each by default); up to `SAMPLED_TEXTURES` textures a stage; and its memory
/// in blocks of `MEMORY_BLOCKS`.
fn with_features(default: DeviceDescriptor) -> DeviceDescriptor {
    Arc::new(move |adapter| {
        let mut descriptor = default(adapter);
        descriptor.required_features |= adapter.features() & WANTED;
        descriptor.memory_hints = wgpu::MemoryHints::Manual {
            suballocated_device_memory_block_size: MEMORY_BLOCKS,
        };
        let offered = adapter.limits();
        let limits = &mut descriptor.required_limits;
        limits.max_texture_array_layers = limits.max_texture_array_layers.max(offered.max_texture_array_layers);
        limits.max_sampled_textures_per_shader_stage = limits
            .max_sampled_textures_per_shader_stage
            .max(offered.max_sampled_textures_per_shader_stage.min(SAMPLED_TEXTURES));
        descriptor
    })
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    use uniwow_api::wgpu;

    use super::{MEMORY_BLOCKS, SAMPLED_TEXTURES, WANTED, with_features};

    fn resolved<F: Future>(future: F) -> Option<F::Output> {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        }
    }

    #[test]
    fn the_device_asks_for_the_features_the_layers_and_the_textures_the_adapter_offers_in_blocks_of_memory_of_its_own()
    {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let Some(Ok(adapter)) = resolved(instance.request_adapter(&wgpu::RequestAdapterOptions {
            force_fallback_adapter: true,
            ..Default::default()
        })) else {
            eprintln!("skipped: no software adapter");
            return;
        };
        let default = Arc::new(|_: &wgpu::Adapter| wgpu::DeviceDescriptor {
            label: Some("default"),
            ..Default::default()
        });
        let descriptor = with_features(default)(&adapter);
        for feature in WANTED.iter() {
            let offered = adapter.features().contains(feature);
            assert_eq!(descriptor.required_features.contains(feature), offered, "{feature:?}");
        }
        assert_eq!(
            descriptor.required_limits.max_texture_array_layers,
            adapter.limits().max_texture_array_layers
        );
        assert_eq!(
            descriptor.required_limits.max_sampled_textures_per_shader_stage,
            adapter
                .limits()
                .max_sampled_textures_per_shader_stage
                .min(SAMPLED_TEXTURES)
        );
        assert!(
            matches!(
                &descriptor.memory_hints,
                wgpu::MemoryHints::Manual { suballocated_device_memory_block_size } if *suballocated_device_memory_block_size == MEMORY_BLOCKS
            ),
            "{:?}",
            descriptor.memory_hints
        );
        // The device made as asked.
        let made = resolved(adapter.request_device(&descriptor));
        assert!(
            made.is_some_and(|made| made.is_ok()),
            "the adapter gives what is asked of it"
        );
        assert_eq!(descriptor.label, Some("default"), "the rest as by default");
        eprintln!(
            "the software adapter offers {:?}, {} layers an array",
            adapter.features() & WANTED,
            adapter.limits().max_texture_array_layers
        );
    }
}
