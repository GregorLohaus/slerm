mod app;
mod config;
mod fonts;
mod input;
mod layout;
mod pane;
mod pty;
mod render;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let mut path = config::Config::path()?;
    let mut check = false;
    let mut init = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => path = args.next().context("--config needs a path")?.into(),
            "--check-config" => check = true,
            "--init-config" => init = true,
            "--version" => {
                println!("slerm {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!(
                    "slerm [--config PATH] [--check-config | --init-config]\n\nDefault config: {}",
                    path.display()
                );
                return Ok(());
            }
            _ => bail!("Unknown argument: {arg}"),
        }
    }
    if init {
        use std::io::Write;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!(
                    "Cannot create {} (existing files are never overwritten)",
                    path.display()
                )
            })?;
        file.write_all(config::EXAMPLE.as_bytes())?;
        println!("Created {}", path.display());
        return Ok(());
    }
    let config = config::Config::load(&path)?;
    if check {
        println!(
            "Configuration valid: {}{}",
            path.display(),
            if path.exists() {
                ""
            } else {
                " (built-in defaults)"
            }
        );
        return Ok(());
    }
    libghostty_vt::kitty::graphics::set_png_decoder(Some(Box::new(render::PngDecoder)))?;
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("slerm")
        .with_inner_size([config.window_width, config.window_height])
        .with_min_inner_size([320.0, 200.0])
        .with_app_id("slerm");
    if cfg!(target_os = "macos") {
        // Hide the titlebar and draw under it; the traffic lights stay on top of
        // the tab bar, which reserves room for them (see app::TITLEBAR_INSET).
        viewport = viewport
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "slerm",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, config, path)?))),
    )
    .map_err(|e| anyhow::anyhow!("Cannot start slerm: {e}"))
}
