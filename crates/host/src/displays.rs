//! Human-readable monitor names through the DisplayConfig API.

use std::collections::HashMap;

use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::ERROR_SUCCESS;

fn wide(s: &[u16]) -> String {
    let len = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf16_lossy(&s[..len])
}

/// Maps GDI device names (`\\.\DISPLAY1`) to (friendly name, is virtual).
pub fn friendly_names() -> HashMap<String, (String, bool)> {
    let mut map = HashMap::new();
    let (mut np, mut nm) = (0u32, 0u32);
    unsafe {
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return map;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return map;
        }
        paths.truncate(np as usize);
        for p in &paths {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = p.sourceInfo.adapterId;
            source.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = p.targetInfo.adapterId;
            target.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut target.header) != 0 {
                continue;
            }
            let tech = target.outputTechnology;
            let path = wide(&target.monitorDevicePath).to_ascii_uppercase();
            let is_virtual = tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL
                || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_WIRED
                || path.contains("MTT");
            let mut name = wide(&target.monitorFriendlyDeviceName);
            if name.is_empty() {
                name = if tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL {
                    "Built-in display".into()
                } else if is_virtual {
                    "Virtual display".into()
                } else {
                    "Display".into()
                };
            }
            map.insert(wide(&source.viewGdiDeviceName), (name, is_virtual));
        }
    }
    map
}
