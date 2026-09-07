use anyhow::Result;
use libghostty_vt::{Terminal, key};

pub fn mods(m: egui::Modifiers) -> key::Mods {
    let mut out = key::Mods::empty();
    if m.ctrl {
        out |= key::Mods::CTRL;
    }
    if m.shift {
        out |= key::Mods::SHIFT;
    }
    if m.alt {
        out |= key::Mods::ALT;
    }
    if m.mac_cmd {
        out |= key::Mods::SUPER;
    }
    out
}

pub fn encode(terminal: &Terminal<'_, '_>, events: &[egui::Event]) -> Result<Vec<u8>> {
    let mut encoder = key::Encoder::new()?;
    encoder.set_options_from_terminal(terminal);
    let mut event = key::Event::new()?;
    let mut out = vec![];
    let mut used_text = std::collections::HashSet::new();
    for (index, e) in events.iter().enumerate() {
        match e {
            egui::Event::Key {
                key: k,
                physical_key,
                pressed,
                repeat,
                modifiers,
            } => {
                let (logical, ucp) = map(*k);
                let (physical, _) = map(physical_key.unwrap_or(*k));
                let mut text = None;
                if *pressed && printable(*k) {
                    // winit emits text immediately after its key event. Stop at the next key
                    // so a burst of distinct keys cannot consume each other's text.
                    for (i, next) in events.iter().enumerate().skip(index + 1) {
                        if matches!(next, egui::Event::Key { .. }) {
                            break;
                        }
                        if let egui::Event::Text(value) = next {
                            text = Some(value.as_str());
                            used_text.insert(i);
                            break;
                        }
                    }
                }
                event
                    .set_action(if !pressed {
                        key::Action::Release
                    } else if *repeat {
                        key::Action::Repeat
                    } else {
                        key::Action::Press
                    })
                    .set_key(if physical == key::Key::Unidentified {
                        logical
                    } else {
                        physical
                    })
                    .set_mods(mods(*modifiers))
                    .set_consumed_mods(if text.is_some() {
                        key::Mods::SHIFT
                    } else {
                        key::Mods::empty()
                    })
                    .set_unshifted_codepoint(ucp)
                    .set_utf8(text);
                encoder.encode_to_vec(&event, &mut out)?;
            }
            egui::Event::Text(text) if !used_text.contains(&index) => {
                event
                    .set_action(key::Action::Press)
                    .set_key(key::Key::Unidentified)
                    .set_mods(key::Mods::empty())
                    .set_consumed_mods(key::Mods::empty())
                    .set_unshifted_codepoint('\0')
                    .set_utf8(Some(text.as_str()));
                encoder.encode_to_vec(&event, &mut out)?;
            }
            egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                out.extend_from_slice(text.as_bytes())
            }
            _ => {}
        }
    }
    Ok(out)
}

fn printable(k: egui::Key) -> bool {
    let (_, c) = map(k);
    c != '\0'
}

fn map(k: egui::Key) -> (key::Key, char) {
    use egui::Key as E;
    use key::Key as G;
    match k {
        E::A => (G::A, 'a'),
        E::B => (G::B, 'b'),
        E::C => (G::C, 'c'),
        E::D => (G::D, 'd'),
        E::E => (G::E, 'e'),
        E::F => (G::F, 'f'),
        E::G => (G::G, 'g'),
        E::H => (G::H, 'h'),
        E::I => (G::I, 'i'),
        E::J => (G::J, 'j'),
        E::K => (G::K, 'k'),
        E::L => (G::L, 'l'),
        E::M => (G::M, 'm'),
        E::N => (G::N, 'n'),
        E::O => (G::O, 'o'),
        E::P => (G::P, 'p'),
        E::Q => (G::Q, 'q'),
        E::R => (G::R, 'r'),
        E::S => (G::S, 's'),
        E::T => (G::T, 't'),
        E::U => (G::U, 'u'),
        E::V => (G::V, 'v'),
        E::W => (G::W, 'w'),
        E::X => (G::X, 'x'),
        E::Y => (G::Y, 'y'),
        E::Z => (G::Z, 'z'),
        E::Num0 => (G::Digit0, '0'),
        E::Num1 => (G::Digit1, '1'),
        E::Num2 => (G::Digit2, '2'),
        E::Num3 => (G::Digit3, '3'),
        E::Num4 => (G::Digit4, '4'),
        E::Num5 => (G::Digit5, '5'),
        E::Num6 => (G::Digit6, '6'),
        E::Num7 => (G::Digit7, '7'),
        E::Num8 => (G::Digit8, '8'),
        E::Num9 => (G::Digit9, '9'),
        E::Space => (G::Space, ' '),
        E::Minus => (G::Minus, '-'),
        E::Equals => (G::Equal, '='),
        E::Comma => (G::Comma, ','),
        E::Period => (G::Period, '.'),
        E::Slash => (G::Slash, '/'),
        E::Backslash => (G::Backslash, '\\'),
        E::OpenBracket => (G::BracketLeft, '['),
        E::CloseBracket => (G::BracketRight, ']'),
        E::Backtick => (G::Backquote, '`'),
        E::Semicolon => (G::Semicolon, ';'),
        E::Quote => (G::Quote, '\''),
        E::ArrowDown => (G::ArrowDown, '\0'),
        E::ArrowUp => (G::ArrowUp, '\0'),
        E::ArrowLeft => (G::ArrowLeft, '\0'),
        E::ArrowRight => (G::ArrowRight, '\0'),
        E::Enter => (G::Enter, '\0'),
        E::Escape => (G::Escape, '\0'),
        E::Tab => (G::Tab, '\0'),
        E::Backspace => (G::Backspace, '\0'),
        E::Delete => (G::Delete, '\0'),
        E::Insert => (G::Insert, '\0'),
        E::Home => (G::Home, '\0'),
        E::End => (G::End, '\0'),
        E::PageUp => (G::PageUp, '\0'),
        E::PageDown => (G::PageDown, '\0'),
        E::F1 => (G::F1, '\0'),
        E::F2 => (G::F2, '\0'),
        E::F3 => (G::F3, '\0'),
        E::F4 => (G::F4, '\0'),
        E::F5 => (G::F5, '\0'),
        E::F6 => (G::F6, '\0'),
        E::F7 => (G::F7, '\0'),
        E::F8 => (G::F8, '\0'),
        E::F9 => (G::F9, '\0'),
        E::F10 => (G::F10, '\0'),
        E::F11 => (G::F11, '\0'),
        E::F12 => (G::F12, '\0'),
        _ => (G::Unidentified, '\0'),
    }
}
