#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Public Folders — a lightweight viewer for Microsoft Exchange public
//! folders, for people whose new Outlook no longer shows them.
//!
//! Options:
//!   --demo                 open with demo folders (no server needed)
//!   --demo-folder <id>     open the demo at this folder (e.g. cal-rooms)
//!   --screenshot <file>    draw ~120 frames, save the window as PNG, exit

mod app;
mod cache;
mod classic;
#[cfg(windows)]
mod com;
mod demo;
#[cfg(windows)]
mod dnd;
mod export;
#[cfg(windows)]
mod mapi;
mod ews;
mod model;
mod ntlm;
mod oauth;
mod settings;
mod winauth;

use eframe::egui;
use std::path::PathBuf;

fn icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let inside_rounded = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let cx = x.clamp(x0 + r, x1 - r);
        let cy = y.clamp(y0 + r, y1 - r);
        x >= x0 && x <= x1 && y >= y0 && y <= y1 && (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let px = &mut rgba[(y * n + x) * 4..(y * n + x) * 4 + 4];
            if inside_rounded(fx, fy, 2.0, 2.0, 62.0, 62.0, 14.0) {
                px.copy_from_slice(&[15, 108, 189, 255]);
                if inside_rounded(fx, fy, 14.0, 18.0, 32.0, 28.0, 3.0) {
                    px.copy_from_slice(&[190, 222, 250, 255]);
                }
                if inside_rounded(fx, fy, 14.0, 24.0, 50.0, 48.0, 4.0) {
                    px.copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    }
    egui::IconData { rgba, width: n as u32, height: n as u32 }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut shot: Option<PathBuf> = None;
    let mut demo: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--screenshot" => {
                shot = args.get(i + 1).map(PathBuf::from);
                i += 1;
            }
            "--demo" => demo = Some("ann".into()),
            "--demo-folder" => {
                demo = args.get(i + 1).cloned();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Public Folders")
        .with_inner_size([1240.0, 780.0])
        .with_min_inner_size([860.0, 560.0])
        .with_icon(icon());
    if shot.is_some() {
        // Don't take focus from whatever the person is doing.
        viewport = viewport.with_active(false);
        #[cfg(target_os = "windows")]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }

    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions { viewport, ..Default::default() };
    #[cfg(target_os = "macos")]
    if shot.is_some() {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            builder.with_activate_ignoring_other_apps(false);
        }));
    }

    eframe::run_native(
        "Public Folders",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, shot, demo)))),
    )
}
