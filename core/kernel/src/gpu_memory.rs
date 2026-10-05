//! The memory of the GPU the editor draws with: wgpu does not tell it; DXGI does, for the adapter
//! of the same vendor and device, whichever backend wgpu uses.

use uniwow_api::wgpu;

/// The memory of its own of the adapter `info` describes, in bytes; `None` when the system does
/// not tell it, as for a software adapter, which has none.
pub fn dedicated(info: &wgpu::AdapterInfo) -> Option<u64> {
    #[cfg(windows)]
    {
        dxgi(info.vendor, info.device)
    }
    #[cfg(not(windows))]
    {
        let _ = info;
        None
    }
}

#[cfg(windows)]
fn dxgi(vendor: u32, device: u32) -> Option<u64> {
    use uniwow_api::windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

    // SAFETY: calls of DXGI with no argument but an index, their errors checked.
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.ok()?;
    (0..)
        .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
        .filter_map(|adapter| unsafe { adapter.GetDesc1() }.ok())
        .find(|desc| desc.VendorId == vendor && desc.DeviceId == device)
        .map(|desc| desc.DedicatedVideoMemory as u64)
        .filter(|bytes| *bytes > 0)
}

#[cfg(all(test, windows))]
mod tests {
    use uniwow_api::windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};

    #[test]
    fn each_adapter_is_found_by_its_ids_and_none_by_others() {
        assert_eq!(super::dxgi(0xFFFF, 0xFFFF), None, "no adapter has these ids");
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.expect("DXGI");
        let descs: Vec<_> = (0..)
            .map_while(|index| unsafe { factory.EnumAdapters1(index) }.ok())
            .map(|adapter| unsafe { adapter.GetDesc1() }.expect("description"))
            .collect();
        assert!(!descs.is_empty(), "Windows has at least its software adapter");
        for desc in descs {
            let memory = super::dxgi(desc.VendorId, desc.DeviceId);
            if desc.DedicatedVideoMemory == 0 {
                assert_eq!(memory, None, "a software adapter has no memory of its own");
            } else {
                assert!(memory.is_some_and(|bytes| bytes > 0));
            }
        }
    }
}
