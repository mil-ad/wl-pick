//! What a tile stands for: a window, or a whole display.
//!
//! Both are capture sources as far as the protocol is concerned — one from a
//! foreign-toplevel handle, one from a `wl_output` — so the grid treats them
//! alike and only differs in how it labels them and what picking one does.

use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Window,
    Output,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Window => "window",
            Kind::Output => "output",
        })
    }
}

#[derive(Clone)]
pub struct Target {
    pub kind: Kind,
    /// The thing a caller acts on: a sway `con_id` for a window, an output name
    /// for a display.
    pub id: String,
    /// sway container id, when there is one.
    pub con_id: Option<i64>,
    /// ext-foreign-toplevel-list identifier, for matching a capture source.
    pub ft_id: String,
    pub app: String,
    pub title: String,
}

impl Target {
    pub fn window(con_id: i64, ft_id: String, app: String, title: String) -> Self {
        Self {
            kind: Kind::Window,
            id: con_id.to_string(),
            con_id: Some(con_id),
            ft_id,
            app,
            title,
        }
    }

    /// A display. `app` is "display" so the label says what kind of tile it is
    /// without needing a second visual language for it.
    pub fn output(name: String) -> Self {
        Self {
            kind: Kind::Output,
            id: name.clone(),
            con_id: None,
            ft_id: String::new(),
            app: "display".to_string(),
            title: name,
        }
    }

    /// "title · app", the label the rofi grid used.
    pub fn label(&self) -> String {
        if self.app.is_empty() {
            self.title.clone()
        } else {
            format!("{} · {}", self.title, self.app)
        }
    }

    /// One tab-separated row, so a shell caller can
    /// `IFS=$'\t' read -r type id toplevel app title`.
    ///
    /// Both identifiers are there because both get used: sway scripting acts on
    /// the con_id (`[con_id=N] focus`), while tools that capture a window —
    /// grim -T, the desktop portal — want the foreign-toplevel identifier.
    pub fn tsv(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.kind,
            self.id,
            self.ft_id,
            clean(&self.app),
            clean(&self.title)
        )
    }

    /// The same record as JSON, with every key always present so `jq` can rely
    /// on it. Written by hand: one object is not worth a serialiser.
    pub fn json(&self) -> String {
        let opt = |v: Option<String>| match v {
            Some(s) => format!("\"{}\"", esc(&s)),
            None => "null".to_string(),
        };
        let (con_id, output) = match self.kind {
            Kind::Window => (
                self.con_id.map(|n| n.to_string()).unwrap_or("null".into()),
                "null".to_string(),
            ),
            Kind::Output => ("null".to_string(), opt(Some(self.id.clone()))),
        };
        let toplevel = match self.ft_id.is_empty() {
            true => "null".to_string(),
            false => format!("\"{}\"", esc(&self.ft_id)),
        };
        format!(
            "{{\"type\":\"{}\",\"con_id\":{con_id},\"toplevel_id\":{toplevel},\
             \"output\":{output},\"app\":\"{}\",\"title\":\"{}\"}}",
            self.kind,
            esc(&self.app),
            esc(&self.title)
        )
    }

    /// What xdg-desktop-portal-wlr's `simple` chooser accepts: `Monitor: NAME`
    /// or `Window: <foreign-toplevel identifier>`. A window the compositor never
    /// gave an identifier for cannot be named this way, hence the Option — and
    /// an empty stdout is exactly how that chooser says "declined".
    pub fn portal(&self) -> Option<String> {
        match self.kind {
            Kind::Output => Some(format!("Monitor: {}", self.id)),
            Kind::Window if !self.ft_id.is_empty() => Some(format!("Window: {}", self.ft_id)),
            Kind::Window => None,
        }
    }
}

/// Titles are arbitrary application strings; a tab or newline in one would split
/// the row a caller is parsing.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| {
            if (c as u32) < 0x20 || c == '\u{7f}' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// JSON string escaping, per RFC 8259.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win() -> Target {
        Target::window(42, "abc123".into(), "kitty".into(), "zsh\tin\na tab".into())
    }

    #[test]
    fn tsv_is_one_line_with_both_identifiers() {
        let row = win().tsv();
        assert_eq!(row, "window\t42\tabc123\tkitty\tzsh in a tab");
        assert_eq!(row.split('\t').count(), 5);
        assert!(!row.contains('\n'));
    }

    #[test]
    fn json_keeps_every_key_and_escapes() {
        let j = win().json();
        assert!(j.contains("\"type\":\"window\""), "{j}");
        assert!(j.contains("\"con_id\":42"), "{j}");
        assert!(j.contains("\"toplevel_id\":\"abc123\""), "{j}");
        assert!(j.contains("\"output\":null"), "{j}");
        // Control characters survive as escapes, not raw bytes.
        assert!(j.contains("zsh\\tin\\na tab"), "{j}");

        let o = Target::output("DP-1".into()).json();
        assert!(o.contains("\"con_id\":null"), "{o}");
        assert!(o.contains("\"output\":\"DP-1\""), "{o}");
    }

    #[test]
    fn portal_speaks_xdpw() {
        assert_eq!(win().portal().as_deref(), Some("Window: abc123"));
        assert_eq!(
            Target::output("DP-1".into()).portal().as_deref(),
            Some("Monitor: DP-1")
        );
        // No identifier means the portal cannot be told about this window.
        let anon = Target::window(7, String::new(), "x".into(), "y".into());
        assert_eq!(anon.portal(), None);
    }

    #[test]
    fn displays_say_what_they_are() {
        let t = Target::output("DP-1".into());
        assert_eq!(t.label(), "DP-1 · display");
        assert_eq!(t.con_id, None);
    }
}
