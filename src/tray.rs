use std::process::Command;
use tracing::{error, info};

use crate::audio::list_input_devices;

/// Spawn the system tray icon on a dedicated OS thread.
/// The tray shows available audio input devices and restarts the process on selection.
pub fn spawn_tray(current_device: Option<String>) {
    std::thread::spawn(move || {
        if let Err(e) = run_tray(current_device) {
            error!("System tray error: {}", e);
        }
    });
}

fn run_tray(current_device: Option<String>) -> anyhow::Result<()> {
    use muda::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::TrayIconBuilder;

    let menu = Menu::new();

    // Build device submenu
    let devices_submenu = Submenu::new("Audio Input", true);
    let devices = list_input_devices();

    let mut device_items: Vec<(MenuItem, String)> = Vec::new();
    for (name, is_default) in &devices {
        let is_active = match &current_device {
            Some(selected) => selected == name,
            None => *is_default,
        };
        let label = if is_active {
            format!("* {}", name)
        } else if *is_default {
            format!("  {} (default)", name)
        } else {
            format!("  {}", name)
        };
        let item = MenuItem::new(label, true, None);
        devices_submenu.append(&item)?;
        device_items.push((item, name.clone()));
    }

    menu.append(&devices_submenu)?;
    menu.append(&PredefinedMenuItem::separator())?;

    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&quit_item)?;

    // Create a simple 16x16 icon (blue square)
    let icon = create_icon();

    let _tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("WHCanRC Assisted Listening")
        .with_icon(icon)
        .build()?;

    info!("System tray icon created");

    // Run the event loop
    let menu_rx = MenuEvent::receiver();

    loop {
        // On Windows we need a message pump; on Linux GTK handles it.
        // Use a simple polling loop with a short sleep.
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

        // Check for menu events
        if let Ok(event) = menu_rx.try_recv() {
            // Check if quit was clicked
            if event.id() == quit_item.id() {
                info!("Quit requested from tray");
                std::process::exit(0);
            }

            // Check if a device was selected
            for (item, device_name) in &device_items {
                if event.id() == item.id() {
                    restart_with_device(device_name);
                }
            }
        }

        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn restart_with_device(device_name: &str) {
    info!("Restarting with device: {}", device_name);

    let exe = std::env::current_exe().expect("Failed to get current exe path");
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // Remove existing --device and its value
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--device" {
            args.remove(i);
            if i < args.len() {
                args.remove(i);
            }
        } else if args[i].starts_with("--device=") {
            args.remove(i);
        } else {
            i += 1;
        }
    }

    // Add new device selection
    args.push("--device".to_string());
    args.push(device_name.to_string());

    match Command::new(&exe).args(&args).spawn() {
        Ok(_) => {
            info!("New instance started, exiting current process");
            std::process::exit(0);
        }
        Err(e) => {
            error!("Failed to restart: {}", e);
        }
    }
}

fn create_icon() -> tray_icon::Icon {
    // 16x16 RGBA icon — a simple filled blue/green circle
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
                rgba[idx] = 74;      // R
                rgba[idx + 1] = 144;  // G
                rgba[idx + 2] = 217;  // B
                rgba[idx + 3] = 255;  // A
            }
        }
    }

    tray_icon::Icon::from_rgba(rgba, size, size).expect("Failed to create tray icon")
}
