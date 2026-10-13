//! Ossenbloed: per mode a pane palette and an oxblood chrome palette (toolbar, folders, status bar) around it, and the
//! grid every size comes from. `ui/theme.slint` holds the same values for the window; a test keeps the two equal.

/// A colour as 0xRRGGBB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Colour(pub u32);

impl Colour {
    pub(crate) fn rgb(self) -> [u8; 3] {
        let [_, red, green, blue] = self.0.to_be_bytes();
        [red, green, blue]
    }
}

pub(crate) struct Palette {
    pub accent: Colour,
    pub background: Colour,
    pub border: Colour,
    pub error: Colour,
    pub faint: Colour,
    /// The reader's subject.
    pub heading: Colour,
    pub highlight: Colour,
    pub hover: Colour,
    pub muted: Colour,
    /// Text on `selected`.
    pub on_accent: Colour,
    pub panel: Colour,
    /// Behind a widget while it is pressed or dragged; `strong` is drawn on it.
    pub pressed: Colour,
    pub secondary: Colour,
    /// Behind the selected row, folder and text.
    pub selected: Colour,
    /// Bold text, such as account headings and header labels: more contrast than `text`, and it must also read on
    /// `pressed`.
    pub strong: Colour,
    pub text: Colour,
    pub warning: Colour,
}

/// Height of buttons and fields, and the width of icon buttons.
pub(crate) const CONTROL: f32 = 28.0;
/// Height of the toolbar.
pub(crate) const TOP_BAR: f32 = 40.0;
/// Height of rows, text buttons and fields.
pub(crate) const ROW: f32 = 24.0;
/// Space between a row's edge and its content; a pane's edge and its rows are 8 px apart too, so content sits 16 px in.
pub(crate) const PAD: f32 = 8.0;
/// Space between a pane's edge and content that is not in a row.
pub(crate) const INSET: f32 = 2.0 * PAD;
/// Size of the icons on icon buttons.
pub(crate) const ICON: f32 = 16.0;
/// Space between an icon button's edge and its glyph.
pub(crate) const GLYPH: f32 = (CONTROL - ICON) / 2.0;
/// Corner radius of the panes where they meet the frame.
pub(crate) const RADIUS: f32 = 10.0;

/// Light panes.
pub(crate) const PAPER: Palette = Palette {
    accent: Colour(0x9a4a1c),
    background: Colour(0xfbf8f3),
    border: Colour(0xe3d8cb),
    error: Colour(0xa8202e),
    faint: Colour(0xfffdfa),
    heading: Colour(0x3b1418),
    highlight: Colour(0x8c6720),
    hover: Colour(0xeadfd0),
    muted: Colour(0x6e5958),
    on_accent: Colour(0xf1e4d8),
    panel: Colour(0xf3ece2),
    pressed: Colour(0xe3d8cb),
    secondary: Colour(0x5e4a4b),
    selected: Colour(0x3b1418),
    strong: Colour(0x140b0c),
    text: Colour(0x2b1a1c),
    warning: Colour(0x8a5a00),
};

/// Dark panes.
pub(crate) const NIGHT: Palette = Palette {
    accent: Colour(0xe9c9b0),
    background: Colour(0x231919),
    border: Colour(0x3a2b2c),
    error: Colour(0xf08a8a),
    faint: Colour(0x140e0f),
    heading: Colour(0xe9c9b0),
    highlight: Colour(0xc99a3e),
    hover: Colour(0x2e2223),
    muted: Colour(0xb09a93),
    on_accent: Colour(0xf1e4d8),
    panel: Colour(0x1a1213),
    pressed: Colour(0x3a2b2c),
    secondary: Colour(0xcdb8af),
    selected: Colour(0x5a1f25),
    strong: Colour(0xffffff),
    text: Colour(0xefe3d6),
    warning: Colour(0xe6b85c),
};

/// The oxblood chrome around the light panes.
pub(crate) const FRAME: Palette = Palette {
    accent: Colour(0xd9b36a),
    background: Colour(0x3b1418),
    border: Colour(0x5a2a2f),
    error: Colour(0xffb4a8),
    faint: Colour(0x4e2629),
    heading: Colour(0xf1e4d8),
    highlight: Colour(0xd9b36a),
    hover: Colour(0x4a1d22),
    muted: Colour(0xc29a92),
    on_accent: Colour(0xffffff),
    panel: Colour(0x3b1418),
    pressed: Colour(0x5e2a30),
    secondary: Colour(0xe0cbbe),
    selected: Colour(0x552a2b),
    strong: Colour(0xffffff),
    text: Colour(0xf1e4d8),
    warning: Colour(0xf2cc7a),
};

/// The chrome around the dark panes.
pub(crate) const FRAME_NIGHT: Palette = Palette {
    accent: Colour(0xd9b36a),
    background: Colour(0x2a0e11),
    border: Colour(0x4a2226),
    error: Colour(0xffb4a8),
    faint: Colour(0x3b1d20),
    heading: Colour(0xf1e4d8),
    highlight: Colour(0xd9b36a),
    hover: Colour(0x3a171b),
    muted: Colour(0xc29a92),
    on_accent: Colour(0xffffff),
    panel: Colour(0x2a0e11),
    pressed: Colour(0x4c2227),
    secondary: Colour(0xe0cbbe),
    selected: Colour(0x44262a),
    strong: Colour(0xffffff),
    text: Colour(0xf1e4d8),
    warning: Colour(0xf2cc7a),
};

impl Palette {
    /// Every colour with its field name, as `ui/theme.slint` names it.
    pub(crate) fn named(&self) -> [(&'static str, Colour); 17] {
        [
            ("accent", self.accent),
            ("background", self.background),
            ("border", self.border),
            ("error", self.error),
            ("faint", self.faint),
            ("heading", self.heading),
            ("highlight", self.highlight),
            ("hover", self.hover),
            ("muted", self.muted),
            ("on-accent", self.on_accent),
            ("panel", self.panel),
            ("pressed", self.pressed),
            ("secondary", self.secondary),
            ("selected", self.selected),
            ("strong", self.strong),
            ("text", self.text),
            ("warning", self.warning),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [&Palette; 4] = [&PAPER, &NIGHT, &FRAME, &FRAME_NIGHT];

    /// WCAG contrast ratio between two opaque colours, 1 to 21.
    fn contrast(a: Colour, b: Colour) -> f32 {
        let luminance = |c: Colour| {
            let channel = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.040_45 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            let [r, g, b] = c.rgb();
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        };
        let (light, dark) = (
            luminance(a).max(luminance(b)),
            luminance(a).min(luminance(b)),
        );
        (light + 0.05) / (dark + 0.05)
    }

    /// Regression for #41: bold text in the dark theme was drawn in `on_accent`, near black on the dark panels.
    #[test]
    fn strong_text_stands_out_from_normal_text_in_every_palette() {
        for p in ALL {
            for background in [p.panel, p.background] {
                let normal = contrast(p.text, background);
                let bold = contrast(p.strong, background);
                assert!(bold >= 7.0, "{:?}: strong text contrast {bold}", p.panel);
                assert!(bold > normal, "{:?}: strong {bold} <= normal {normal}", p.panel);
            }
            let pressed = contrast(p.strong, p.pressed);
            assert!(pressed >= 4.5, "{:?}: pressed widget contrast {pressed}", p.panel);
        }
    }

    #[test]
    fn text_on_the_selection_links_and_status_text_pass_aa_in_every_palette() {
        for p in ALL {
            let name = p.panel;
            let selected = contrast(p.on_accent, p.selected);
            assert!(selected >= 4.5, "{name:?}: text on the selection {selected}");
            let heading = contrast(p.heading, p.background);
            assert!(heading >= 4.5, "{name:?}: heading {heading}");
            let link = contrast(p.accent, p.background);
            assert!(link >= 4.5, "{name:?}: accent text {link}");
            for (what, color) in [("muted", p.muted), ("error", p.error)] {
                let ratio = contrast(color, p.panel).min(contrast(color, p.hover));
                assert!(ratio >= 4.5, "{name:?}: {what} text {ratio}");
            }
            let marks = contrast(p.highlight, p.panel);
            assert!(marks >= 3.0, "{name:?}: highlight marks {marks}");
        }
    }

    /// Regression: light links were oxblood, which next to the near-black ink read as plain text.
    #[test]
    fn links_stand_apart_from_text_in_every_palette() {
        for p in ALL {
            let apart = contrast(p.accent, p.text);
            assert!(apart >= 1.2, "{:?}: link against text {apart}", p.panel);
        }
    }

    /// Selected text in a field is drawn in `on_accent` on `selected`.
    #[test]
    fn selected_text_in_text_fields_is_readable_in_every_palette() {
        for p in ALL {
            let ratio = contrast(p.on_accent, p.selected);
            assert!(ratio >= 4.5, "{:?}: selected text contrast {ratio}", p.panel);
        }
    }
}
