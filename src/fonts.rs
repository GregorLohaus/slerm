use crate::config::Config;
use anyhow::{Context, Result, ensure};
use egui::{FontData, FontDefinitions, FontFamily};
use fontdb::{Database, Family, ID, Query, Style, Weight};
use std::{collections::HashMap, path::Path};

pub fn family(bold: bool, italic: bool) -> FontFamily {
    match (bold, italic) {
        (false, false) => FontFamily::Monospace,
        (true, false) => FontFamily::Name("terminal-bold".into()),
        (false, true) => FontFamily::Name("terminal-italic".into()),
        (true, true) => FontFamily::Name("terminal-bold-italic".into()),
    }
}

fn query(db: &Database, name: &str, bold: bool, italic: bool) -> Option<ID> {
    db.query(&Query {
        families: &[Family::Name(name)],
        weight: if bold { Weight::BOLD } else { Weight::NORMAL },
        style: if italic { Style::Italic } else { Style::Normal },
        ..Default::default()
    })
}

fn load_file(db: &mut Database, path: &Path) -> Result<ID> {
    let bytes =
        std::fs::read(path).with_context(|| format!("Cannot read font {}", path.display()))?;
    db.load_font_source(fontdb::Source::Binary(std::sync::Arc::new(bytes)))
        .first()
        .copied()
        .with_context(|| format!("No font faces in {}", path.display()))
}

fn register(
    db: &Database,
    fonts: &mut FontDefinitions,
    loaded: &mut HashMap<ID, String>,
    id: ID,
) -> Result<String> {
    if let Some(name) = loaded.get(&id) {
        return Ok(name.clone());
    }
    let face = db.face(id).context("Font face disappeared")?;
    let data = db
        .with_face_data(id, |bytes, index| {
            // Validate with the renderer's parser too, so a bad font becomes a
            // reload error rather than a panic inside egui on the next frame.
            ab_glyph::FontRef::try_from_slice_and_index(bytes, index)
                .with_context(|| format!("Unsupported font: {}", face.post_script_name))?;
            let mut data = FontData::from_owned(bytes.to_vec());
            data.index = index;
            Ok::<_, anyhow::Error>(data)
        })
        .context("Cannot read font face data")??;
    let name = format!("terminal-face-{}", loaded.len());
    fonts.font_data.insert(name.clone(), data.into());
    loaded.insert(id, name.clone());
    Ok(name)
}

pub fn load(config: &Config) -> Result<FontDefinitions> {
    let mut db = Database::new();
    // Preserve the built-in default when no family is configured. System
    // fonts also supply optional Nerd Font symbols and named fallbacks.
    db.load_system_fonts();
    load_from_database(config, db)
}

fn load_from_database(config: &Config, mut db: Database) -> Result<FontDefinitions> {
    let mut primary = config.font_family.clone();
    let regular = if let Some(path) = &config.font_path {
        let id = load_file(&mut db, path)?;
        primary = db
            .face(id)
            .and_then(|face| face.families.first())
            .map(|f| f.0.clone());
        // Inspect sibling metadata, not filename suffixes: foundries use
        // different style names, and collections contain multiple faces.
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        for entry in std::fs::read_dir(parent)?.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                ["ttf", "otf", "ttc", "otc"]
                    .iter()
                    .any(|ext| e.eq_ignore_ascii_case(ext))
            }) {
                let _ = db.load_font_file(path);
            }
        }
        Some(id)
    } else if let Some(name) = &primary {
        Some(
            query(&db, name, false, false)
                .with_context(|| format!("Font family not found: {name}"))?,
        )
    } else {
        None
    };
    for name in &config.font_fallbacks {
        ensure!(
            query(&db, name, false, false).is_some(),
            "Fallback font family not found: {name}"
        );
    }
    let mut fallbacks = config.font_fallbacks.clone();
    for name in ["Symbols Nerd Font Mono", "Symbols Nerd Font"] {
        if query(&db, name, false, false).is_some() && !fallbacks.iter().any(|f| f == name) {
            fallbacks.push(name.into());
            break;
        }
    }
    let mut fonts = FontDefinitions::default();
    let builtin = fonts.families[&FontFamily::Monospace].clone();
    let mut loaded = HashMap::new();
    // Nerd Fonts cover private-use icons, but can omit ordinary Unicode
    // symbols (including ✓ and ✔). Keep system symbol faces after the user's
    // fonts, and never let them establish the terminal's line metrics.
    let mut symbols = Vec::new();
    for name in [
        "Apple Symbols",
        "Noto Sans Symbols 2",
        "Noto Sans Symbols",
        "Segoe UI Symbol",
        "DejaVu Sans",
        "Arial Unicode MS",
    ] {
        if let Some(id) = query(&db, name, false, false)
            && let Ok(name) = register(&db, &mut fonts, &mut loaded, id)
        {
            symbols.push(name);
        }
    }
    for (bold, italic, path) in [
        (false, false, None),
        (true, false, config.font_bold_path.as_deref()),
        (false, true, config.font_italic_path.as_deref()),
        (true, true, config.font_bold_italic_path.as_deref()),
    ] {
        let id = if let Some(path) = path {
            Some(load_file(&mut db, path)?)
        } else if !bold && !italic {
            regular
        } else {
            primary
                .as_ref()
                .and_then(|name| query(&db, name, bold, italic))
                .or(regular)
        };
        let mut chain = Vec::new();
        if let Some(id) = id {
            chain.push(register(&db, &mut fonts, &mut loaded, id)?);
        } else {
            // A symbols-only fallback must not set the terminal's line metrics.
            chain.push(builtin[0].clone());
        }
        // A style face may lack symbols present in its regular face.
        if let Some(id) = regular {
            let name = register(&db, &mut fonts, &mut loaded, id)?;
            if !chain.contains(&name) {
                chain.push(name);
            }
        }
        for name in &fallbacks {
            for id in [
                query(&db, name, bold, italic),
                query(&db, name, false, false),
            ]
            .into_iter()
            .flatten()
            {
                let name = register(&db, &mut fonts, &mut loaded, id)?;
                if !chain.contains(&name) {
                    chain.push(name);
                }
            }
        }
        for name in &symbols {
            if !chain.contains(name) {
                chain.push(name.clone());
            }
        }
        chain.extend(builtin.iter().cloned());
        fonts.families.insert(family(bold, italic), chain);
    }
    Ok(fonts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Database {
        let mut db = Database::new();
        let fonts = FontDefinitions::default();
        let data = &fonts.font_data[&fonts.families[&FontFamily::Monospace][0]];
        db.load_font_data(data.font.to_vec());
        let template = db.faces().next().unwrap().clone();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut face = template.clone();
            face.families[0].0 = "Test Family".into();
            face.weight = if bold { Weight::BOLD } else { Weight::NORMAL };
            face.style = if italic { Style::Italic } else { Style::Normal };
            db.push_face_info(face);
        }
        db
    }

    #[test]
    fn matches_styles_and_keeps_regular_as_fallback() {
        let config = Config {
            font_family: Some("Test Family".into()),
            ..Default::default()
        };
        let db = database();
        let mut ids = std::collections::HashSet::new();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let id = query(&db, "Test Family", bold, italic).unwrap();
            assert!(ids.insert(id), "styles must select distinct faces");
            let face = db.face(id).unwrap();
            assert_eq!(face.weight == Weight::BOLD, bold);
            assert_eq!(face.style == Style::Italic, italic);
        }
        let fonts = load_from_database(&config, db).unwrap();
        let regular = &fonts.families[&family(false, false)][0];
        for (bold, italic) in [(true, false), (false, true), (true, true)] {
            let chain = &fonts.families[&family(bold, italic)];
            assert_ne!(&chain[0], regular);
            assert_eq!(&chain[1], regular);
        }
    }

    #[test]
    fn rejects_missing_families_and_accepts_regular_only_fallback() {
        let mut config = Config {
            font_family: Some("Not Installed".into()),
            ..Default::default()
        };
        assert!(load_from_database(&config, database()).is_err());
        config.font_family = None;
        config.font_fallbacks = vec!["Not Installed".into()];
        assert!(load_from_database(&config, database()).is_err());
        let db = database();
        let name = db.faces().next().unwrap().families[0].0.clone();
        config.font_fallbacks = vec![name];
        let fonts = load_from_database(&config, db).unwrap();
        assert_eq!(
            fonts.families[&family(false, false)],
            fonts.families[&family(true, true)]
        );
    }

    #[test]
    #[ignore = "requires SLERM_TEST_FONT pointing to a Nerd Font regular face with sibling styles"]
    fn installed_nerd_font_family_and_sibling_styles() {
        use ab_glyph::Font;
        let path = std::env::var_os("SLERM_TEST_FONT").expect("set SLERM_TEST_FONT");
        let config = Config {
            font_path: Some(path.into()),
            ..Default::default()
        };
        let fonts = load(&config).unwrap();
        let mut faces = std::collections::HashSet::new();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let name = &fonts.families[&family(bold, italic)][0];
            assert!(faces.insert(name.clone()));
            let data = &fonts.font_data[name];
            let face = ab_glyph::FontRef::try_from_slice_and_index(&data.font, data.index).unwrap();
            for c in [
                'M',
                '\u{f00c}',
                '\u{f42e}',
                '\u{f012c}',
                '\u{f121}',
                '\u{e0a0}',
                '\u{f0001}',
            ] {
                assert_ne!(face.glyph_id(c).0, 0, "missing {c:?} in {name}");
            }
            for c in ['✓', '✔', '☑'] {
                assert!(
                    fonts.families[&family(bold, italic)].iter().any(|name| {
                        let data = &fonts.font_data[name];
                        ab_glyph::FontRef::try_from_slice_and_index(&data.font, data.index)
                            .unwrap()
                            .glyph_id(c)
                            .0
                            != 0
                    }),
                    "missing fallback for {c:?}"
                );
            }
        }
        let mut db = Database::new();
        let id = load_file(&mut db, config.font_path.as_ref().unwrap()).unwrap();
        let name = db.face(id).unwrap().families[0].0.clone();
        eprintln!("Verified installed family: {name}");
        let named = load(&Config {
            font_family: Some(name),
            ..Default::default()
        })
        .unwrap();
        for (bold, italic) in [(false, false), (true, false), (false, true), (true, true)] {
            let a = &fonts.font_data[&fonts.families[&family(bold, italic)][0]];
            let b = &named.font_data[&named.families[&family(bold, italic)][0]];
            assert_eq!(a.font, b.font);
            assert_eq!(a.index, b.index);
        }
    }
}
