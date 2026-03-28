use tracing::{error, info};

use crate::audio::{list_input_devices, DeviceSwitcher};

/// Spawn the system tray icon on a dedicated OS thread.
/// If a DeviceSwitcher is provided, selecting a device switches live.
/// Otherwise (e.g. test-tone mode), the device menu is shown but disabled.
pub fn spawn_tray(
    current_device: Option<String>,
    switcher: Option<DeviceSwitcher>,
    url: String,
) {
    std::thread::spawn(move || {
        if let Err(e) = run_tray(current_device, switcher, url) {
            error!("System tray error: {}", e);
        }
    });
}

fn run_tray(
    mut current_device: Option<String>,
    switcher: Option<DeviceSwitcher>,
    url: String,
) -> anyhow::Result<()> {
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::TrayIconBuilder;

    let menu = Menu::new();

    // Build device submenu
    let devices_submenu = Submenu::new("Audio Input", true);
    let devices = list_input_devices();

    let device_items = build_device_menu(&devices_submenu, &devices, &current_device)?;

    menu.append(&devices_submenu)?;
    menu.append(&PredefinedMenuItem::separator())?;

    let qr_item = MenuItem::new("Show QR Code", true, None);
    menu.append(&qr_item)?;

    let copy_url_item = MenuItem::new("Copy URL", true, None);
    menu.append(&copy_url_item)?;

    menu.append(&PredefinedMenuItem::separator())?;

    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&quit_item)?;

    let icon = create_icon();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("WHCanRC Assisted Listening")
        .with_icon(icon)
        .build()?;

    info!("System tray icon created");

    let menu_rx = MenuEvent::receiver();

    loop {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::UI::WindowsAndMessaging::{
                DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
            };
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).into() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }

        if let Ok(event) = menu_rx.try_recv() {
            if event.id() == quit_item.id() {
                info!("Quit requested from tray");
                std::process::exit(0);
            }

            if event.id() == qr_item.id() {
                show_qr_code(&url);
            }

            if event.id() == copy_url_item.id() {
                copy_to_clipboard(&url);
            }

            for (item, device_name) in &device_items {
                if event.id() == item.id() {
                    if let Some(ref switcher) = switcher {
                        info!("Switching to device: {}", device_name);
                        switcher.switch_device(device_name.clone());
                        current_device = Some(device_name.clone());

                        // Update menu labels to reflect new selection
                        let devices = list_input_devices();
                        update_device_labels(&device_items, &devices, &current_device);
                    }
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn build_device_menu(
    submenu: &muda::Submenu,
    devices: &[(String, bool)],
    current_device: &Option<String>,
) -> anyhow::Result<Vec<(muda::MenuItem, String)>> {
    let mut items = Vec::new();
    for (name, is_default) in devices {
        let is_active = match current_device {
            Some(ref selected) => selected == name,
            None => *is_default,
        };
        let label = format_device_label(name, is_active, *is_default);
        let item = muda::MenuItem::new(label, true, None);
        submenu.append(&item)?;
        items.push((item, name.clone()));
    }
    Ok(items)
}

fn update_device_labels(
    items: &[(muda::MenuItem, String)],
    devices: &[(String, bool)],
    current_device: &Option<String>,
) {
    for (item, name) in items {
        let is_default = devices.iter().any(|(n, d)| n == name && *d);
        let is_active = match current_device {
            Some(ref selected) => selected == name,
            None => is_default,
        };
        item.set_text(format_device_label(name, is_active, is_default));
    }
}

fn format_device_label(name: &str, is_active: bool, is_default: bool) -> String {
    if is_active {
        format!("* {}", name)
    } else if is_default {
        format!("  {} (default)", name)
    } else {
        format!("  {}", name)
    }
}

fn show_qr_code(url: &str) {
    use qrcode::QrCode;

    let qr = match QrCode::new(url.as_bytes()) {
        Ok(qr) => qr,
        Err(e) => {
            error!("Failed to generate QR code: {}", e);
            return;
        }
    };

    let svg = qr
        .render::<qrcode::render::svg::Color>()
        .quiet_zone(true)
        .min_dimensions(200, 200)
        .build();

    let html = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Connect — WHCanRC</title>
<style>
  body {{ display:flex; flex-direction:column; align-items:center; justify-content:center;
         height:100vh; margin:0; font-family:system-ui,sans-serif; background:#f5f5f5; }}
  a {{ font-size:1.2em; margin-top:1em; color:#2563eb; }}
</style></head>
<body>{svg}<a href="{url}">{url}</a></body></html>"#,
        svg = svg,
        url = url
    );

    let path = std::env::temp_dir().join("whcanrc_qr.html");
    if let Err(e) = std::fs::write(&path, html) {
        error!("Failed to write QR code file: {}", e);
        return;
    }

    info!("Opening QR code in browser");
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", &path.to_string_lossy()])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&path).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    }
}

fn copy_to_clipboard(text: &str) {
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", &format!("echo|set /p={}|clip", text)])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(text.as_bytes());
            }
        }
    }
    info!("URL copied to clipboard");
}

fn create_icon() -> tray_icon::Icon {
    let size = 16u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let center = size as f32 / 2.0;
    let radius = center - 1.0;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = ((y * size + x) * 4) as usize;
            if dist <= radius {
                rgba[idx] = 74;
                rgba[idx + 1] = 144;
                rgba[idx + 2] = 217;
                rgba[idx + 3] = 255;
            }
        }
    }

    tray_icon::Icon::from_rgba(rgba, size, size).expect("Failed to create tray icon")
}
