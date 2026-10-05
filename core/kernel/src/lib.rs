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
        setup.device_descriptor = with_block_compression(setup.device_descriptor.clone());
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

/// The device as `default` asks for it, with the block compressions of the textures of WoW (BC1
/// to BC3) when the adapter offers them: the textures then go to the GPU as they are stored.
fn with_block_compression(default: DeviceDescriptor) -> DeviceDescriptor {
    Arc::new(move |adapter| {
        let mut descriptor = default(adapter);
        descriptor.required_features |= adapter.features() & wgpu::Features::TEXTURE_COMPRESSION_BC;
        descriptor
    })
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::sync::Arc;
    use std::task::{Context, Poll, Waker};

    use uniwow_api::wgpu;

    use super::with_block_compression;

    fn resolved<F: Future>(future: F) -> Option<F::Output> {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(value) => Some(value),
            Poll::Pending => None,
        }
    }

    #[test]
    fn the_device_asks_for_block_compression_when_the_adapter_offers_it() {
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
        let descriptor = with_block_compression(default)(&adapter);
        let offered = adapter.features().contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        assert_eq!(
            descriptor
                .required_features
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            offered
        );
        assert_eq!(descriptor.label, Some("default"), "the rest as by default");
        eprintln!("the software adapter offers BC: {offered}");
    }
}
