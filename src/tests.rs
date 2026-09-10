use crate::{config::Config, input, pane, render, theme};
use base64::Engine;
use egui::{Event, Key, Modifiers};
use libghostty_vt::{
    fmt,
    kitty::graphics::{self, ImageFormat, PlacementIterator},
};
use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

fn plain(term: &libghostty_vt::Terminal<'_, '_>) -> String {
    let mut formatter =
        fmt::Formatter::new(term, fmt::FormatterOptions::new().with_trim(true)).unwrap();
    let bytes = formatter.format_alloc(None).unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn cursor_defaults_to_hollow_and_respects_application_styles() {
    use libghostty_vt::{RenderState, render::CursorVisualStyle};

    let mut term = pane::terminal(&Config::default(), &theme::DEFAULT).unwrap();
    let mut state = RenderState::new().unwrap();
    let snapshot = state.update(&term).unwrap();
    assert_eq!(
        snapshot.cursor_visual_style().unwrap(),
        CursorVisualStyle::BlockHollow
    );
    for (sequence, expected) in [
        (b"\x1b[2 q", CursorVisualStyle::Block),
        (b"\x1b[4 q", CursorVisualStyle::Underline),
        (b"\x1b[6 q", CursorVisualStyle::Bar),
        (b"\x1b[0 q", CursorVisualStyle::BlockHollow),
    ] {
        term.vt_write(sequence);
        let snapshot = state.update(&term).unwrap();
        assert_eq!(snapshot.cursor_visual_style().unwrap(), expected);
    }
}

#[test]
fn terminal_preserves_unicode_colors_and_application_input_modes() {
    let mut term = pane::terminal(&Config::default(), &theme::DEFAULT).unwrap();
    term.vt_write("hello \x1b[31mred\x1b[0m \u{754c}e\u{301}\r\n".as_bytes());
    assert!(plain(&term).contains("hello red \u{754c}e\u{301}"));
    let events = [
        key(Key::A, Modifiers::NONE),
        Event::Text("a".into()),
        key(Key::B, Modifiers::NONE),
        Event::Text("b".into()),
    ];
    assert_eq!(input::encode(&term, &events).unwrap(), b"ab");
    assert_eq!(
        input::encode(&term, &[key(Key::C, Modifiers::CTRL)]).unwrap(),
        b"\x03"
    );
    assert_eq!(
        input::encode(&term, &[key(Key::ArrowUp, Modifiers::NONE)]).unwrap(),
        b"\x1b[A"
    );
    term.vt_write(b"\x1b[?1h");
    assert_eq!(
        input::encode(&term, &[key(Key::ArrowUp, Modifiers::NONE)]).unwrap(),
        b"\x1bOA"
    );
    term.resize(12, 6, 9, 20).unwrap();
    assert!(plain(&term).contains("hello red"));
}

#[test]
fn input_encoding_grows_buffers_without_losing_events() {
    let term = pane::terminal(&Config::default(), &theme::DEFAULT).unwrap();
    let mut events = vec![Event::Text("a".into()), Event::Text("123456789".into())];
    let long_text = "界".repeat(200);
    events.push(Event::Text(long_text.clone()));
    for _ in 0..100 {
        events.push(key(Key::ArrowUp, Modifiers::NONE));
    }
    let expected = format!("a123456789{long_text}{}", "\x1b[A".repeat(100));
    assert_eq!(input::encode(&term, &events).unwrap(), expected.as_bytes());
}

#[test]
fn mouse_encoding_appends_when_destination_has_insufficient_spare_capacity() {
    use libghostty_vt::mouse;

    let mut term = pane::terminal(&Config::default(), &theme::DEFAULT).unwrap();
    term.vt_write(b"\x1b[?1000h\x1b[?1006h");
    let mut encoder = mouse::Encoder::new().unwrap();
    encoder
        .set_options_from_terminal(&term)
        .set_size(mouse::EncoderSize {
            screen_width: 800,
            screen_height: 600,
            cell_width: 8,
            cell_height: 16,
            padding_top: 0,
            padding_bottom: 0,
            padding_left: 0,
            padding_right: 0,
        });
    let mut event = mouse::Event::new().unwrap();
    event
        .set_action(mouse::Action::Press)
        .set_button(Some(mouse::Button::Left))
        .set_position(mouse::Position { x: 0.0, y: 0.0 });
    let mut out = Vec::with_capacity(16);
    out.resize(9, b'x');
    for _ in 0..100 {
        input::append_encoded(&mut out, |buffer| encoder.encode(&event, buffer)).unwrap();
    }
    assert_eq!(
        out,
        format!("xxxxxxxxx{}", "\x1b[<0;1;1M".repeat(100)).as_bytes()
    );
}

#[test]
fn queries_return_to_the_pty_and_alternate_screen_restores() {
    let mut term = pane::terminal(&Config::default(), &theme::DEFAULT).unwrap();
    let response = Rc::new(RefCell::new(vec![]));
    let writer = response.clone();
    term.on_pty_write(move |_, bytes| writer.borrow_mut().extend_from_slice(bytes))
        .unwrap();
    term.vt_write(b"hello\x1b[6n");
    assert_eq!(*response.borrow(), b"\x1b[1;6R");
    term.vt_write(b"\x1b[?1049hother screen\x1b[?1049l");
    assert!(plain(&term).contains("hello"));
    assert!(!plain(&term).contains("other screen"));
}

#[test]
fn kitty_images_decode_render_reuse_and_delete() {
    graphics::set_png_decoder(Some(Box::new(render::PngDecoder))).unwrap();
    let mut term = pane::terminal(&Config::default(), &theme::DEFAULT).unwrap();
    let replies = Rc::new(RefCell::new(vec![]));
    let output = replies.clone();
    term.on_pty_write(move |_, data| output.borrow_mut().extend_from_slice(data))
        .unwrap();
    // Split delivery across VT writes, including the control string terminator.
    term.vt_write(b"\x1b_Ga=T,f=24,s=2,v=1,i=7,c=4,r=2;//8A");
    term.vt_write(b"AP8A\x1b\\");
    assert!(String::from_utf8_lossy(&replies.borrow()).contains("OK"));
    {
        let graphics = term.kitty_graphics().unwrap();
        let image = graphics.image(7).expect("RGB image stored");
        assert_eq!(image.format().unwrap(), ImageFormat::Rgb);
        assert_eq!(image.data().unwrap(), [255, 255, 0, 0, 255, 0]);
        let mut it = PlacementIterator::new().unwrap();
        let mut ps = it.update(&graphics).unwrap();
        let info = ps
            .next()
            .unwrap()
            .placement_render_info(&image, &term)
            .unwrap();
        assert!(info.viewport_visible);
        assert_eq!((info.pixel_width, info.pixel_height), (32, 32));
    }
    let mut png = vec![];
    {
        let mut encoder = png::Encoder::new(&mut png, 2, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[255, 0, 0, 0, 255, 0])
            .unwrap();
    }
    let payload = base64::engine::general_purpose::STANDARD.encode(png);
    let split = payload.len() / 8 * 4;
    term.vt_write(
        format!(
            "\x1b_Ga=T,f=100,i=8,c=2,r=1,m=1;{}\x1b\\",
            &payload[..split]
        )
        .as_bytes(),
    );
    term.vt_write(format!("\x1b_Gm=0;{}\x1b\\", &payload[split..]).as_bytes());
    {
        let graphics = term.kitty_graphics().unwrap();
        let image = graphics.image(8).expect("PNG image stored");
        assert_eq!(image.format().unwrap(), ImageFormat::Rgba);
        assert_eq!(image.data().unwrap(), [255, 0, 0, 255, 0, 255, 0, 255]);
    }
    let ctx = egui::Context::default();
    let mut renderer = render::Renderer::new().unwrap();
    let rect = egui::Rect::from_min_size(egui::pos2(50.0, 60.0), egui::vec2(400.0, 240.0));
    let mut paint = || {
        ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx
                .layer_painter(egui::LayerId::background())
                .with_clip_rect(rect);
            renderer
                .paint(
                    &term,
                    &painter,
                    &render::Grid {
                        rect,
                        cell: egui::vec2(8.0, 16.0),
                        font: &egui::FontId::monospace(12.0),
                        theme: &theme::DEFAULT,
                        active: true,
                    },
                )
                .unwrap();
        })
    };
    let first = paint();
    assert_eq!(
        first
            .textures_delta
            .set
            .iter()
            .filter(|(id, _)| matches!(id,egui::TextureId::Managed(n) if *n>0))
            .count(),
        2
    );
    assert!(first.shapes.iter().all(|s| rect.contains_rect(s.clip_rect)));
    let second = paint();
    assert!(
        second.textures_delta.set.is_empty(),
        "unchanged images must reuse GPU textures"
    );
    term.vt_write(b"\x1b_Ga=d,d=I,i=7\x1b\\");
    assert!(term.kitty_graphics().unwrap().image(7).is_none());
    assert!(term.kitty_graphics().unwrap().image(8).is_some());
}

#[test]
fn pty_executes_input_and_bracketed_paste_is_sanitized() {
    let config = Config {
        shell: vec!["/bin/sh".into()],
        ..Default::default()
    };
    let ctx = egui::Context::default();
    let mut pane = pane::Pane::new(&config, &theme::DEFAULT, &ctx, None).unwrap();
    pane.send(b"printf '\\033[31mpty-%s\\033[0m\\n' 'success'\r".to_vec())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        pane.drain(&ctx);
        if plain(&pane.terminal).contains("pty-success") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(plain(&pane.terminal).contains("pty-success"));
    pane.pty.resize(91, 31, 9, 20).unwrap();
    pane.send(b"stty size\r".to_vec()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        pane.drain(&ctx);
        if plain(&pane.terminal).contains("31 91") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(plain(&pane.terminal).contains("31 91"));
    pane.send(b"exit\r".to_vec()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pane.dead && Instant::now() < deadline {
        pane.drain(&ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(pane.dead);
    assert!(pane.send(b"after exit".to_vec()).is_err());
}

#[test]
fn new_panes_start_in_the_directory_of_the_pane_they_came_from() {
    let config = Config {
        shell: vec!["/bin/sh".into()],
        ..Default::default()
    };
    let ctx = egui::Context::default();
    let dir = std::env::temp_dir().join(format!("slerm cwd {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.canonicalize().unwrap();
    let mut pane = pane::Pane::new(&config, &theme::DEFAULT, &ctx, None).unwrap();
    pane.send(format!("cd '{}'\r", dir.display()).into_bytes())
        .unwrap();
    // /bin/sh reports no OSC 7, so the directory has to come from the process.
    let deadline = Instant::now() + Duration::from_secs(5);
    while pane.cwd().as_deref() != Some(dir.as_path()) && Instant::now() < deadline {
        pane.drain(&ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(pane.cwd().as_deref(), Some(dir.as_path()));

    let mut child = pane::Pane::new(&config, &theme::DEFAULT, &ctx, pane.cwd().as_deref()).unwrap();
    child
        .send(b"printf 'at:%s\\n' \"$PWD\"\r".to_vec())
        .unwrap();
    let expected = format!("at:{}", dir.display());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !plain(&child.terminal).contains(&expected) && Instant::now() < deadline {
        child.drain(&ctx);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(plain(&child.terminal).contains(&expected));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn osc7_wins_over_the_process_directory_but_only_for_this_machine() {
    let config = Config {
        shell: vec!["/bin/sh".into()],
        ..Default::default()
    };
    let ctx = egui::Context::default();
    let reported = std::env::temp_dir().join(format!("slerm osc7 {}", std::process::id()));
    std::fs::create_dir_all(&reported).unwrap();
    let reported = reported.canonicalize().unwrap();
    let mut pane = pane::Pane::new(&config, &theme::DEFAULT, &ctx, None).unwrap();
    let process_cwd = pane.cwd().unwrap();
    assert_ne!(process_cwd, reported);

    let escaped = reported.display().to_string().replace(' ', "%20");
    pane.terminal
        .vt_write(format!("\x1b]7;file://localhost{escaped}\x1b\\").as_bytes());
    assert_eq!(pane.cwd(), Some(reported.clone()));

    // A directory on another host cannot be entered here, so the process wins again.
    pane.terminal
        .vt_write(format!("\x1b]7;file://elsewhere{escaped}\x1b\\").as_bytes());
    assert_eq!(pane.cwd(), Some(process_cwd));
    std::fs::remove_dir_all(&reported).unwrap();
}
