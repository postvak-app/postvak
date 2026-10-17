//! The sizes the Rust side needs from the grid; the grid and the palettes are `Theme` in `ui/theme.slint`. The tests
//! here keep the two equal and check the palettes' contrast.

/// Height of buttons and fields, and the width of icon buttons.
pub(crate) const CONTROL: f32 = 28.0;
/// Height of a message in the list: the sender and date, then the subject.
pub(crate) const LIST_ROW: f32 = 52.0;

#[cfg(test)]
mod tests {
    use slint::{Color, ComponentHandle};

    use super::*;
    use crate::ui::{Palette, StartupWindow, Theme};

    /// The four palettes as the window has them: the panes light and dark, the frame light and dark.
    fn palettes() -> [Palette; 4] {
        crate::test_support::headless();
        let window = StartupWindow::new().unwrap();
        let theme = window.global::<Theme>();
        [
            theme.get_paper(),
            theme.get_night(),
            theme.get_frame_day(),
            theme.get_frame_night(),
        ]
    }

    /// WCAG contrast ratio between two opaque colours, 1 to 21.
    fn contrast(a: Color, b: Color) -> f32 {
        let luminance = |c: Color| {
            let channel = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.040_45 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            let (r, g, b) = (c.red(), c.green(), c.blue());
            0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
        };
        let (light, dark) = (
            luminance(a).max(luminance(b)),
            luminance(a).min(luminance(b)),
        );
        (light + 0.05) / (dark + 0.05)
    }

    #[test]
    fn the_slint_theme_has_the_same_grid() {
        crate::test_support::headless();
        let window = StartupWindow::new().unwrap();
        let theme = window.global::<Theme>();
        assert_eq!(theme.get_control(), CONTROL);
        assert_eq!(theme.get_list_row(), LIST_ROW);
    }

    #[test]
    fn the_system_theme_follows_the_desktop_and_a_choice_overrides_it() {
        use crate::ui::{MailWindow, ThemePreference};
        crate::test_support::headless();
        let window = MailWindow::new().unwrap();
        let theme = window.global::<Theme>();
        theme.set_preference(ThemePreference::System);
        theme.set_desktop_dark(true);
        assert!(theme.get_dark());
        theme.set_desktop_dark(false);
        assert!(!theme.get_dark());
        theme.set_preference(ThemePreference::Dark);
        assert!(theme.get_dark());
        theme.set_desktop_dark(true);
        theme.set_preference(ThemePreference::Light);
        assert!(!theme.get_dark());
    }

    /// Regression for #41: bold text in the dark theme was drawn in `on_accent`, near black on the dark panels.
    #[test]
    fn strong_text_stands_out_from_normal_text_in_every_palette() {
        for p in palettes() {
            for background in [p.panel, p.background] {
                let normal = contrast(p.text, background);
                let bold = contrast(p.strong, background);
                assert!(bold >= 7.0, "{:?}: strong text contrast {bold}", p.panel);
                assert!(
                    bold > normal,
                    "{:?}: strong {bold} <= normal {normal}",
                    p.panel
                );
            }
            let pressed = contrast(p.strong, p.pressed);
            assert!(
                pressed >= 4.5,
                "{:?}: pressed widget contrast {pressed}",
                p.panel
            );
        }
    }

    #[test]
    fn text_on_its_backgrounds_passes_aa_in_every_palette() {
        for p in palettes() {
            let name = p.panel;
            let selected = contrast(p.on_accent, p.selected);
            assert!(
                selected >= 4.5,
                "{name:?}: text on the selection {selected}"
            );
            let heading = contrast(p.heading, p.background);
            assert!(heading >= 4.5, "{name:?}: heading {heading}");
            let link = contrast(p.accent, p.background);
            assert!(link >= 4.5, "{name:?}: accent text {link}");
            for (what, color) in [("muted", p.muted), ("error", p.error)] {
                let ratio = contrast(color, p.panel).min(contrast(color, p.hover));
                assert!(ratio >= 4.5, "{name:?}: {what} text {ratio}");
            }
            let pairs = [
                ("text", p.text, p.panel),
                ("text", p.text, p.background),
                ("text on hover", p.text, p.hover),
                ("text on the selection", p.text, p.selected),
                ("secondary on the selection", p.secondary, p.selected),
                ("warning", p.warning, p.background),
                ("highlight text", p.highlight, p.background),
            ];
            for (what, text, background) in pairs {
                let ratio = contrast(text, background);
                assert!(ratio >= 4.5, "{name:?}: {what} {ratio}");
            }
            let marks = contrast(p.highlight, p.panel);
            assert!(marks >= 3.0, "{name:?}: highlight marks {marks}");
        }
    }

    /// Regression: light links were oxblood, which next to the near-black ink read as plain text.
    #[test]
    fn links_stand_apart_from_text_in_every_palette() {
        for p in palettes() {
            let apart = contrast(p.accent, p.text);
            assert!(apart >= 1.2, "{:?}: link against text {apart}", p.panel);
        }
    }

    /// Selected text in a field is drawn in `on_accent` on `selected`.
    #[test]
    fn selected_text_in_text_fields_is_readable_in_every_palette() {
        for p in palettes() {
            let ratio = contrast(p.on_accent, p.selected);
            assert!(
                ratio >= 4.5,
                "{:?}: selected text contrast {ratio}",
                p.panel
            );
        }
    }
}
