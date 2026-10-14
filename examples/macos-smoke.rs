//! Native smoke test: cargo run --example macos-smoke (on macOS).
//! Opens a temporary window, exercises AppKit menu dispatch, and exits.
#[cfg(target_os = "macos")]
#[path = "../src/macos.rs"]
mod macos;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use objc2::{MainThreadMarker, msg_send, rc::Retained};
    use objc2_app_kit::{NSApplication, NSMenu};
    use objc2_foundation::ns_string;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    struct Smoke;
    impl eframe::App for Smoke {
        fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
    eframe::run_native(
        "Slerm menu smoke test",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([240.0, 100.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            let opened = Arc::new(AtomicUsize::new(0));
            let callback = opened.clone();
            macos::install(&cc.egui_ctx, move || {
                callback.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })?;
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            let delegate = app.delegate().unwrap();
            // SAFETY: install registered this delegate method with an NSMenu result.
            let dock: Retained<NSMenu> =
                unsafe { msg_send![&delegate, applicationDockMenu: &*app] };
            assert_eq!(
                dock.itemAtIndex(0).unwrap().title().to_string(),
                "New Window"
            );
            assert!(dock.itemAtIndex(0).unwrap().keyEquivalent().is_empty());
            dock.performActionForItemAtIndex(0);
            assert_eq!(opened.load(Ordering::Relaxed), 1);
            let file = app
                .mainMenu()
                .unwrap()
                .itemWithTitle(ns_string!("File"))
                .unwrap()
                .submenu()
                .unwrap();
            assert!(file.itemAtIndex(0).unwrap().keyEquivalent().is_empty());
            file.performActionForItemAtIndex(0);
            assert_eq!(opened.load(Ordering::Relaxed), 2);
            assert!(macos::take_error().is_none());
            assert!(app.applicationIconImage().is_some());
            println!("Dock action, File > New Window, no shortcut interception, and application icon verified");
            Ok(Box::new(Smoke))
        }),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This smoke test requires macOS");
}
