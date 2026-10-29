//! The sizes the Rust side needs from the grid; the grid and the palettes are `Theme` in `ui/theme.slint`. The tests
//! here keep the two equal and check the palettes' contrast.

/// Height of buttons and fields, and the width of icon buttons.
pub(crate) const CONTROL: f32 = 28.0;
/// Height of a message in the list: the sender and date, then the subject.
pub(crate) const LIST_ROW: f32 = 72.0;
/// Height of one line of text in the grid: the "Close matches" strip above a search row.
pub(crate) const ROW: f32 = 24.0;
/// How many avatar fills each mode has.
pub(crate) const TINTS: usize = 6;

/// WCAG relative luminance of an opaque sRGB colour, channels 0 to 1.
pub(crate) fn luminance(rgb: [f32; 3]) -> f32 {
    let channel = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

/// WCAG contrast ratio between two opaque sRGB colours, 1 to 21.
pub(crate) fn contrast(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

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
        let rgb = |c: Color| [c.red(), c.green(), c.blue()].map(|v| f32::from(v) / 255.0);
        super::contrast(rgb(a), rgb(b))
    }

    /// Dark HTML mail sits on the dark pane's background, and its unreadable links take the dark pane's accent.
    #[test]
    fn dark_html_mail_uses_the_dark_pane_colours() {
        use crate::html::dark::{LINK, PAGE};
        let night = palettes()[1].clone();
        let rgb = |c: Color| [c.red(), c.green(), c.blue()].map(|v| f32::from(v) / 255.0);
        for (ours, theirs) in [(PAGE, night.background), (LINK, night.accent)] {
            for (a, b) in ours.iter().zip(rgb(theirs)) {
                assert!((a - b).abs() < 1e-6, "{ours:?} is not {theirs:?}");
            }
        }
    }

    #[test]
    fn the_slint_theme_has_the_same_grid() {
        crate::test_support::headless();
        let window = StartupWindow::new().unwrap();
        let theme = window.global::<Theme>();
        assert_eq!(theme.get_control(), CONTROL);
        assert_eq!(theme.get_list_row(), LIST_ROW);
        assert_eq!(theme.get_row(), ROW);
    }

    #[test]
    fn every_avatar_fill_carries_its_initials_in_both_modes() {
        use slint::Model;
        crate::test_support::headless();
        let window = StartupWindow::new().unwrap();
        let theme = window.global::<Theme>();
        for dark in [false, true] {
            theme.set_preference(if dark {
                crate::ui::ThemePreference::Dark
            } else {
                crate::ui::ThemePreference::Light
            });
            let heading = theme.get_pane().heading;
            let fills = theme.get_avatars();
            assert_eq!(fills.row_count(), TINTS);
            for fill in fills.iter() {
                let ratio = contrast(heading, fill);
                assert!(ratio >= 4.5, "dark {dark}: {fill:?} {ratio}");
            }
        }
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
                ("text on an added line", p.text, p.added),
                ("an added line's mark", p.success, p.added),
            ];
            for (what, text, background) in pairs {
                let ratio = contrast(text, background);
                assert!(ratio >= 4.5, "{name:?}: {what} {ratio}");
            }
            let marks = contrast(p.highlight, p.panel);
            assert!(marks >= 3.0, "{name:?}: highlight marks {marks}");
        }
    }

    /// A primary button's text on the brand fill, hovered or not, and a danger button's on the error fill.
    #[test]
    fn filled_buttons_pass_aa_in_both_modes() {
        crate::test_support::headless();
        let window = StartupWindow::new().unwrap();
        let theme = window.global::<Theme>();
        for fill in [theme.get_brand(), theme.get_brand_hover()] {
            let ratio = contrast(theme.get_on_brand(), fill);
            assert!(ratio >= 4.5, "primary {fill:?}: {ratio}");
        }
        for p in [theme.get_paper(), theme.get_night()] {
            let ratio = contrast(p.faint, p.error);
            assert!(ratio >= 4.5, "danger on {:?}: {ratio}", p.panel);
        }
    }

    /// Regression: light links were once the frame colour, which next to the near-black text read as plain text.
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
