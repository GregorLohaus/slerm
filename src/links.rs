//! Links under the pointer: OSC 8 hyperlinks set by the application, or URLs
//! found in the text, including ones soft-wrapped across rows.
use anyhow::{Context, Result, bail, ensure};
use libghostty_vt::{
    Terminal,
    error::Error,
    screen::{CellWide, GridRef},
    terminal::{Point, PointCoordinate},
};
use std::ops::Range;

/// Schemes slerm opens. Anything else, such as `javascript:` or an app's
/// custom scheme in an OSC 8 link, is ignored rather than handed to the OS.
const SCHEMES: [&str; 5] = ["https://", "http://", "file://", "ftp://", "mailto:"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    /// First and last cell of the link, inclusive, in viewport coordinates.
    pub start: PointCoordinate,
    pub end: PointCoordinate,
    /// Set by the application with OSC 8, so its text may not show the URI.
    pub explicit: bool,
}

impl Link {
    /// The cells the link covers on each viewport row, as `(row, columns)`.
    pub fn rows(&self, cols: u16) -> impl Iterator<Item = (u32, Range<u16>)> + '_ {
        (self.start.y..=self.end.y).map(move |y| {
            let first = if y == self.start.y { self.start.x } else { 0 };
            let last = if y == self.end.y {
                self.end.x
            } else {
                cols - 1
            };
            (y, first..last + 1)
        })
    }
}

/// The link at `point` in a viewport of `cols` x `rows` cells.
pub fn at(
    term: &Terminal<'_, '_>,
    point: PointCoordinate,
    cols: u16,
    rows: u32,
) -> Result<Option<Link>> {
    let cell = |x: u16, y: u32| term.grid_ref(Point::Viewport(PointCoordinate { x, y }));
    if let Some(uri) = hyperlink(&cell(point.x, point.y)?)? {
        // The application marks every cell of the link; extend over the
        // neighbors with the same URI, across soft wraps too.
        let same = |x: u16, y: u32| -> Result<bool> {
            Ok(hyperlink(&cell(x, y)?)?.as_ref() == Some(&uri))
        };
        let (mut start, mut end) = (point, point);
        loop {
            let previous = match (start.x, start.y) {
                (0, 0) => None,
                (0, y) => Some(PointCoordinate {
                    x: cols - 1,
                    y: y - 1,
                }),
                (x, y) => Some(PointCoordinate { x: x - 1, y }),
            };
            match previous {
                Some(p) if same(p.x, p.y)? => start = p,
                _ => break,
            }
        }
        loop {
            let next = if end.x + 1 < cols {
                Some(PointCoordinate {
                    x: end.x + 1,
                    y: end.y,
                })
            } else if end.y + 1 < rows {
                Some(PointCoordinate { x: 0, y: end.y + 1 })
            } else {
                None
            };
            match next {
                Some(p) if same(p.x, p.y)? => end = p,
                _ => break,
            }
        }
        return Ok(openable(&uri).then_some(Link {
            uri,
            start,
            end,
            explicit: true,
        }));
    }

    // The logical line through `point`: soft-wrapped rows read as one.
    let wraps = |y: u32| -> Result<bool> { Ok(cell(0, y)?.row()?.is_wrapped()?) };
    let mut top = point.y;
    while top > 0 && wraps(top - 1)? {
        top -= 1;
    }
    let mut bottom = point.y;
    while bottom + 1 < rows && wraps(bottom)? {
        bottom += 1;
    }
    let mut text = Vec::new();
    let mut cells = Vec::new();
    let mut target = None;
    let mut graphemes = ['\0'; 16];
    for y in top..=bottom {
        for x in 0..cols {
            let grid = cell(x, y)?;
            if matches!(
                grid.cell()?.wide()?,
                CellWide::SpacerTail | CellWide::SpacerHead
            ) {
                if (x, y) == (point.x, point.y) {
                    target = text.len().checked_sub(1);
                }
                continue;
            }
            if (x, y) == (point.x, point.y) {
                target = Some(text.len());
            }
            let count = match grid.graphemes(&mut graphemes) {
                Ok(count) => count,
                Err(Error::OutOfSpace { .. }) => 0,
                Err(e) => return Err(e.into()),
            };
            // A cell is one position however many code points it holds; a
            // combining mark never ends a URL.
            let first = if count == 0 { ' ' } else { graphemes[0] };
            text.push(first);
            cells.push(PointCoordinate { x, y });
        }
    }
    let Some(range) = target.and_then(|target| url_at(&text, target)) else {
        return Ok(None);
    };
    let uri: String = (range.start..range.end)
        .map(|i| {
            // Re-read whole graphemes, so accented characters survive.
            let PointCoordinate { x, y } = cells[i];
            let count = cell(x, y)?.graphemes(&mut graphemes).unwrap_or(0);
            Ok::<_, anyhow::Error>(graphemes[..count.max(1)].iter().collect::<String>())
        })
        .collect::<Result<_>>()?;
    Ok(Some(Link {
        uri,
        start: cells[range.start],
        end: cells[range.end - 1],
        explicit: false,
    }))
}

fn hyperlink(grid: &GridRef<'_>) -> Result<Option<String>> {
    if !grid.cell()?.has_hyperlink()? {
        return Ok(None);
    }
    let mut buf = vec![0; 256];
    let len = match grid.hyperlink_uri(&mut buf) {
        Err(Error::OutOfSpace { required }) => {
            buf.resize(required, 0);
            grid.hyperlink_uri(&mut buf)?
        }
        result => result?,
    };
    buf.truncate(len);
    Ok(
        Some(String::from_utf8(buf).context("Hyperlink is not valid UTF-8")?)
            .filter(|uri| !uri.is_empty()),
    )
}

/// Characters a URL in terminal text can contain. Box drawing and other
/// symbols are excluded, so a URL inside a TUI border stops at the border.
fn url_char(c: char) -> bool {
    if c.is_ascii() {
        c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
    } else {
        c.is_alphanumeric()
    }
}

/// The URL in `text` covering index `at`, if any.
pub fn url_at(text: &[char], at: usize) -> Option<Range<usize>> {
    if !text.get(at).copied().is_some_and(url_char) {
        return None;
    }
    let mut start = at;
    while start > 0 && url_char(text[start - 1]) {
        start -= 1;
    }
    let mut end = at + 1;
    while end < text.len() && url_char(text[end]) {
        end += 1;
    }
    // The last scheme starting at or before the pointer begins the URL.
    let starts_with = |i: usize, scheme: &str| {
        let scheme: Vec<char> = scheme.chars().collect();
        text.get(i..i + scheme.len()).is_some_and(|s| {
            s.iter()
                .zip(&scheme)
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        })
    };
    let (start, scheme) = (start..=at).rev().find_map(|i| {
        SCHEMES
            .iter()
            .find(|s| starts_with(i, s))
            .map(|s| (i, s.len()))
    })?;
    // Sentence punctuation and unbalanced closing brackets around a URL are
    // not part of it; balanced ones, as in Wikipedia links, are.
    while end > start + scheme {
        let last = text[end - 1];
        let unbalanced = |open: char| {
            let count = |c: char| text[start..end].iter().filter(|&&x| x == c).count();
            count(last) > count(open)
        };
        let trim = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' | '*' => true,
            ')' => unbalanced('('),
            ']' => unbalanced('['),
            _ => false,
        };
        if !trim {
            break;
        }
        end -= 1;
    }
    (end > start + scheme && at < end).then_some(start..end)
}

fn openable(uri: &str) -> bool {
    SCHEMES.iter().any(|scheme| {
        uri.get(..scheme.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(scheme))
            && uri.len() > scheme.len()
    }) && !uri.chars().any(char::is_control)
}

/// Opens the link with the system's handler for it, such as the browser.
pub fn open(uri: &str) -> Result<()> {
    ensure!(openable(uri), "Not opening {uri:?}: unsupported link");
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(unix) {
        "xdg-open"
    } else {
        bail!("Opening links is not supported on this platform");
    };
    let mut child = std::process::Command::new(program)
        // Keep the URI an argument: `--` stops `open` reading one that starts
        // with a dash as an option.
        .args(if cfg!(target_os = "macos") {
            &["--"][..]
        } else {
            &[][..]
        })
        .arg(uri)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("Cannot run {program}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(text: &str, at: &str) -> Option<String> {
        let chars: Vec<char> = text.chars().collect();
        let offset = text.find(at).unwrap();
        let at = text[..offset].chars().count();
        url_at(&chars, at).map(|r| chars[r].iter().collect())
    }

    #[test]
    fn urls_are_found_without_surrounding_punctuation() {
        let line =
            "see https://example.com/a?b=c#d, or (https://en.wikipedia.org/wiki/Rust_(language)).";
        assert_eq!(
            find(line, "example").as_deref(),
            Some("https://example.com/a?b=c#d")
        );
        assert_eq!(
            find(line, "https").as_deref(),
            Some("https://example.com/a?b=c#d")
        );
        assert_eq!(
            find(line, "wikipedia").as_deref(),
            Some("https://en.wikipedia.org/wiki/Rust_(language)")
        );
        assert_eq!(find(line, "see"), None);
        assert_eq!(
            find("│https://a.io/x│", "a.io").as_deref(),
            Some("https://a.io/x")
        );
        assert_eq!(
            find("<http://localhost:8080/>", "local").as_deref(),
            Some("http://localhost:8080/")
        );
        assert_eq!(
            find("mail mailto:me@example.com.", "me@").as_deref(),
            Some("mailto:me@example.com")
        );
        assert_eq!(
            find("'file:///tmp/a b'", "tmp").as_deref(),
            Some("file:///tmp/a")
        );
        assert_eq!(find("just https:// here", "https"), None);
        assert_eq!(find("ftp.example.com", "example"), None);
        assert_eq!(
            find("https://bücher.de/ü", "cher").as_deref(),
            Some("https://bücher.de/ü")
        );
    }

    #[test]
    fn only_known_schemes_are_opened() {
        for uri in [
            "https://example.com",
            "HTTP://x",
            "file:///tmp",
            "mailto:a@b.c",
        ] {
            assert!(openable(uri), "{uri}");
        }
        for uri in [
            "javascript:alert(1)",
            "https://",
            "ssh://host",
            "x-man-page://ls",
            "https://a\nb",
            "",
        ] {
            assert!(!openable(uri), "{uri}");
        }
    }

    #[test]
    fn links_are_found_in_the_terminal_across_soft_wraps() {
        let config = crate::config::Config::default();
        let mut term = crate::pane::terminal(&config, &crate::theme::DEFAULT).unwrap();
        term.resize(12, 4, 8, 16).unwrap();
        term.vt_write(b"go https://example.com/path now\r\n");
        let p = |x, y| PointCoordinate { x, y };
        let link = at(&term, p(1, 1), 12, 4).unwrap().unwrap();
        assert_eq!(link.uri, "https://example.com/path");
        assert_eq!(
            (link.start, link.end, link.explicit),
            (p(3, 0), p(2, 2), false)
        );
        assert_eq!(
            link.rows(12).collect::<Vec<_>>(),
            [(0, 3..12), (1, 0..12), (2, 0..3)]
        );
        assert_eq!(at(&term, p(0, 0), 12, 4).unwrap(), None);
        assert_eq!(at(&term, p(6, 2), 12, 4).unwrap(), None);

        // OSC 8: the visible text need not be the URI.
        term.vt_write(
            b"\x1b[2J\x1b[Hsee \x1b]8;;https://example.org/docs\x1b\\the docs\x1b]8;;\x1b\\.",
        );
        let link = at(&term, p(6, 0), 12, 4).unwrap().unwrap();
        assert_eq!(link.uri, "https://example.org/docs");
        assert_eq!(
            (link.start, link.end, link.explicit),
            (p(4, 0), p(11, 0), true)
        );
        assert_eq!(at(&term, p(0, 1), 12, 4).unwrap(), None);
        term.vt_write(b"\x1b[2J\x1b[H\x1b]8;;javascript:alert(1)\x1b\\click\x1b]8;;\x1b\\");
        assert_eq!(at(&term, p(1, 0), 12, 4).unwrap(), None);
    }
}
