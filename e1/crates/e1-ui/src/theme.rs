//! Design tokens.
//!
//! The single source of truth for colour, radius and motion is
//! `assets/themes/*.json`, whose schema is Ginka's (`docs/roadmap.md` E5).
//! Views resolve everything through the `Tokens` global — no view hardcodes
//! a colour, a radius or a duration (`AGENTS.md` rule 5) — because that is
//! what lets a host install its own tokens and have these views render under
//! its theme.

use crate::settings::Appearance;
use gpui::{App, Global, Hsla, Rgba, WindowAppearance};
use serde::{Deserialize, Deserializer};
use std::time::Duration;

const DARK: &str = include_str!("../../../assets/themes/dark.json");
const LIGHT: &str = include_str!("../../../assets/themes/light.json");

/// Light or dark, once the user's choice has been resolved against the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Light.
    Light,
    /// Dark.
    Dark,
}

impl Mode {
    /// Resolve the reader's choice against the window's actual appearance.
    ///
    /// No choice is not a third theme: it is what the window opens on
    /// before anyone has picked, which is whatever the OS is showing, and
    /// the OS answer can change while the app is running.
    pub fn resolve(choice: Option<Appearance>, system: WindowAppearance) -> Self {
        match choice {
            Some(Appearance::Light) => Self::Light,
            Some(Appearance::Dark) => Self::Dark,
            None => match system {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
            },
        }
    }
}

/// The complete token set.
///
/// Fields are exhaustive rather than added on demand: the palette is a
/// contract themes are authored against, and a token that appears only when
/// some view needs it cannot be validated in `assets/themes/`.
#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    /// The theme's own name.
    pub name: String,
    /// Which side of the light/dark line it is on.
    pub appearance: ThemeAppearance,
    /// Every colour.
    pub colors: Colors,
    /// Every radius.
    pub radius: Radii,
    /// Every duration.
    pub duration_ms: Durations,
}

/// What a theme file says about itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAppearance {
    /// Light.
    Light,
    /// Dark.
    #[default]
    Dark,
}

/// Every colour the app may paint.
///
/// `deny_unknown_fields` plus non-optional members means a theme file that
/// misspells or omits a token fails to parse instead of rendering a black
/// hole somewhere in the UI.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    /// Which side of the light/dark line these colours are on, copied from
    /// the theme when it is loaded rather than written in the file.
    ///
    /// The derived fills below need it: a tint that reads as a soft
    /// highlight on a dark ground is a bruise on a light one, and the same
    /// number cannot serve both.
    #[serde(skip)]
    appearance: ThemeAppearance,
    /// The window base, translucent.
    #[serde(rename = "bg.window", deserialize_with = "hex")]
    pub bg_window: Hsla,
    /// The left column.
    #[serde(rename = "bg.sidebar", deserialize_with = "hex")]
    pub bg_sidebar: Hsla,
    /// Cards and fields.
    #[serde(rename = "bg.surface", deserialize_with = "hex")]
    pub bg_surface: Hsla,
    /// Popovers and menus.
    #[serde(rename = "bg.raised", deserialize_with = "hex")]
    pub bg_raised: Hsla,
    /// A terminal pane; unused here, kept for the schema.
    #[serde(rename = "bg.terminal", deserialize_with = "hex")]
    pub bg_terminal: Hsla,
    /// Panel separators.
    #[serde(rename = "border.subtle", deserialize_with = "hex")]
    pub border_subtle: Hsla,
    /// A focused input, a selected row.
    #[serde(rename = "border.strong", deserialize_with = "hex")]
    pub border_strong: Hsla,
    /// Titles and body.
    #[serde(rename = "text.primary", deserialize_with = "hex")]
    pub text_primary: Hsla,
    /// Subtitles and metadata.
    #[serde(rename = "text.secondary", deserialize_with = "hex")]
    pub text_secondary: Hsla,
    /// Timestamps and placeholders.
    #[serde(rename = "text.muted", deserialize_with = "hex")]
    pub text_muted: Hsla,
    /// Selection, links, focus, and here the merged state.
    #[serde(deserialize_with = "hex")]
    pub accent: Hsla,
    /// Something in progress.
    #[serde(rename = "status.working", deserialize_with = "hex")]
    pub status_working: Hsla,
    /// Something waiting on the user.
    #[serde(rename = "status.attention", deserialize_with = "hex")]
    pub status_attention: Hsla,
    /// Done — and here, open: GitHub's own green for an open item.
    #[serde(rename = "status.done", deserialize_with = "hex")]
    pub status_done: Hsla,
    /// Failed — and here, a closed unmerged pull.
    #[serde(rename = "status.error", deserialize_with = "hex")]
    pub status_error: Hsla,
    /// Inline code.
    #[serde(rename = "code.bg", deserialize_with = "hex")]
    pub code_bg: Hsla,
}

/// Every radius.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Radii {
    /// The window's corners.
    pub window: f32,
    /// A card holding a group of controls.
    pub card: f32,
    /// A panel.
    pub panel: f32,
    /// A row, a chip.
    pub row: f32,
}

impl Radii {
    /// A control — a button, a text field — a step under a row: a control
    /// sits inside a card whose corner is the larger one, and matching it
    /// would read as a card in a card.
    pub fn control(&self) -> f32 {
        (self.row - 3.).max(2.)
    }
}

/// Motion durations: ~260 ms for layout, ~120 ms for feedback.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Durations {
    /// Hover and press.
    pub quick: u64,
    /// Content arriving: the fade that carries a fetch's answer onto the
    /// page. Longer than a hover, because it is a thing appearing rather
    /// than a thing reacting; shorter than a panel, because nothing moves.
    pub fade: u64,
    /// Layout and panel transitions.
    pub standard: u64,
}

impl Durations {
    /// Hover and press.
    pub fn quick(&self) -> Duration {
        Duration::from_millis(self.quick)
    }

    /// Content arriving.
    pub fn fade(&self) -> Duration {
        Duration::from_millis(self.fade)
    }

    /// Layout and panel transitions.
    pub fn standard(&self) -> Duration {
        Duration::from_millis(self.standard)
    }
}

impl Tokens {
    /// Copy the active app palette into the shared component contract.
    ///
    /// This preserves host-installed themes when these views are embedded.
    pub fn as_shared(&self) -> bgpui_kit::ThemeTokens {
        bgpui_kit::ThemeTokens {
            name: self.name.clone(),
            appearance: match self.appearance {
                ThemeAppearance::Light => "light",
                ThemeAppearance::Dark => "dark",
            }
            .to_owned(),
            colors: bgpui_kit::Colors {
                bg_window: self.colors.bg_window,
                bg_sidebar: self.colors.bg_sidebar,
                bg_surface: self.colors.bg_surface,
                bg_raised: self.colors.bg_raised,
                bg_terminal: self.colors.bg_terminal,
                border_subtle: self.colors.border_subtle,
                border_strong: self.colors.border_strong,
                text_primary: self.colors.text_primary,
                text_secondary: self.colors.text_secondary,
                text_muted: self.colors.text_muted,
                accent: self.colors.accent,
                status_working: self.colors.status_working,
                status_attention: self.colors.status_attention,
                status_done: self.colors.status_done,
                status_error: self.colors.status_error,
                code_bg: self.colors.code_bg,
            },
            radius: bgpui_kit::Radii {
                window: self.radius.window,
                card: self.radius.card,
                panel: self.radius.panel,
                row: self.radius.row,
            },
            duration_ms: bgpui_kit::Durations {
                quick: self.duration_ms.quick,
                standard: self.duration_ms.standard,
                fade: Some(self.duration_ms.fade),
            },
        }
    }

    /// The built-in theme for a mode.
    pub fn load(mode: Mode) -> Self {
        let source = match mode {
            Mode::Dark => DARK,
            Mode::Light => LIGHT,
        };
        // The themes are compiled in, so a parse failure is a build-time
        // authoring mistake and the tests below catch it.
        let mut tokens: Tokens = serde_json::from_str(source).expect("built-in theme is valid");
        tokens.colors.appearance = tokens.appearance;
        tokens
    }

    /// The installed tokens.
    pub fn global(cx: &App) -> &Tokens {
        cx.global::<Tokens>()
    }

    /// Install a built-in theme.
    pub fn install(mode: Mode, cx: &mut App) {
        cx.set_global(Tokens::load(mode));
    }

    /// The colours.
    pub fn colors(&self) -> &Colors {
        &self.colors
    }

    /// What the logo is painted in: white on the dark theme, navy on the
    /// light one. Not a token, because it is not a colour the rest of the
    /// window uses and a theme file should not have to name the logo.
    pub fn logo(&self) -> Hsla {
        match self.appearance {
            ThemeAppearance::Dark => gpui::white(),
            ThemeAppearance::Light => gpui::rgb(0x1E1B4B).into(),
        }
    }
}

impl Global for Tokens {}

/// Parse `#RRGGBB` or `#RRGGBBAA`.
///
/// Alpha lives in the token rather than at the call site: the sidebar's
/// translucency is a property of the theme, and a view that re-applies
/// opacity on top of it drifts out of spec.
fn hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Hsla, D::Error> {
    use serde::de::Error as _;
    let raw = String::deserialize(deserializer)?;
    parse_hex(&raw).map_err(D::Error::custom)
}

/// Parse `RRGGBB` or `RRGGBBAA`, with or without a leading `#`.
///
/// Public because GitHub sends label colours in the same shape, minus the
/// hash.
pub fn parse_hex(raw: &str) -> Result<Hsla, String> {
    let digits = raw.strip_prefix('#').unwrap_or(raw);
    let (rgb, alpha) = match digits.len() {
        6 => (digits, 0xFF),
        8 => (
            &digits[..6],
            u8::from_str_radix(&digits[6..], 16).map_err(|e| e.to_string())?,
        ),
        other => {
            return Err(format!(
                "expected #RRGGBB or #RRGGBBAA, got {other} digits in {raw:?}"
            ));
        }
    };
    let value = u32::from_str_radix(rgb, 16).map_err(|e| e.to_string())?;
    Ok(Rgba {
        r: ((value >> 16) & 0xFF) as f32 / 255.0,
        g: ((value >> 8) & 0xFF) as f32 / 255.0,
        b: (value & 0xFF) as f32 / 255.0,
        a: alpha as f32 / 255.0,
    }
    .into())
}

/// Push our tokens into `gpui-component`'s theme.
///
/// The toolkit's components resolve their own palette through `cx.theme()`,
/// so a tooltip drawn by the library and a row drawn by us have to agree.
/// Mapping once here is what keeps them from drifting; nothing else in the
/// app should touch `Theme::global_mut`.
pub fn apply(mode: Mode, cx: &mut App) {
    Tokens::install(mode, cx);
    let tokens = *Tokens::global(cx).colors();
    let radii = Tokens::global(cx).radius;

    let theme = gpui_component::Theme::global_mut(cx);
    theme.mode = match mode {
        Mode::Dark => gpui_component::ThemeMode::Dark,
        Mode::Light => gpui_component::ThemeMode::Light,
    };

    theme.colors.background = tokens.bg_window;
    theme.colors.foreground = tokens.text_primary;
    theme.colors.muted = tokens.bg_surface;
    theme.colors.muted_foreground = tokens.text_secondary;
    theme.colors.border = tokens.border_subtle;
    theme.colors.accent = tokens.row_active();
    theme.colors.accent_foreground = tokens.text_primary;
    theme.colors.input = tokens.bg_surface;
    theme.colors.ring = tokens.accent.opacity(0.45);
    theme.colors.selection = tokens.accent.opacity(0.32);
    theme.colors.caret = tokens.accent;

    theme.colors.secondary = tokens.bg_surface;
    theme.colors.secondary_foreground = tokens.text_primary;
    theme.colors.secondary_hover = tokens.surface_hover();
    theme.colors.secondary_active = tokens.surface_hover();

    theme.colors.scrollbar = tokens.transparent_surface();
    theme.colors.scrollbar_thumb = tokens.row_active();
    theme.colors.scrollbar_thumb_hover = tokens.surface_hover();

    theme.colors.sidebar = tokens.bg_sidebar;
    theme.colors.sidebar_foreground = tokens.text_primary;
    theme.colors.sidebar_border = tokens.border_subtle;
    theme.colors.sidebar_accent = tokens.row_active();
    theme.colors.sidebar_accent_foreground = tokens.text_primary;

    // Transparent, not `bg_window`. `Root` already paints the window's
    // translucent background; a second layer of the same colour composites
    // with it — two coats of 82% is 97% — and the glass turns opaque.
    theme.colors.title_bar = gpui::transparent_black();
    theme.colors.title_bar_border = tokens.border_subtle;
    theme.colors.window_border = tokens.border_subtle;

    // A placeholder bar: the row tint, so it reads as the ghost of a row.
    theme.colors.skeleton = tokens.row_hover();
    // Opaque, not the raised surface as authored: a dialog or a menu that
    // floats over text has to hide it, and `bg.raised` lets the glass
    // through. Our own popovers already use `Colors::popover`.
    theme.colors.popover = tokens.popover();
    theme.colors.popover_foreground = tokens.text_primary;
    theme.colors.list = tokens.transparent_surface();
    theme.colors.list_hover = tokens.row_hover();
    theme.colors.list_active = tokens.row_active();
    theme.colors.list_active_border = tokens.accent;

    theme.colors.tab_bar = tokens.transparent_surface();
    theme.colors.tab = tokens.transparent_surface();
    theme.colors.tab_active = tokens.bg_raised;
    theme.colors.tab_foreground = tokens.text_secondary;
    theme.colors.tab_active_foreground = tokens.text_primary;

    // Markdown. A link is the accent, the way a link is in every reader,
    // and never an underline in the muted grey that reads as struck out. A
    // table's head is a darker band over the glass, not a white one: white
    // on this surface is the one colour that is not in the palette.
    theme.colors.link = tokens.accent;
    theme.colors.link_hover = tokens.accent.opacity(0.85);
    theme.colors.link_active = tokens.accent.opacity(0.7);
    theme.colors.table = tokens.transparent_surface();
    theme.colors.table_head = tokens.table_head();
    theme.colors.table_head_foreground = tokens.text_secondary;
    theme.colors.table_foot = tokens.table_head();
    theme.colors.table_foot_foreground = tokens.text_secondary;
    theme.colors.table_even = tokens.transparent_surface();
    theme.colors.table_row_border = tokens.border_subtle;
    theme.colors.table_hover = tokens.row_hover();
    theme.colors.table_active = tokens.row_active();

    theme.colors.primary = tokens.accent;
    theme.colors.primary_foreground = tokens.bg_window;
    theme.colors.primary_hover = tokens.accent.opacity(0.85);
    theme.colors.primary_active = tokens.accent.opacity(0.7);
    theme.colors.danger = tokens.status_error;
    theme.colors.success = tokens.status_done;

    theme.radius = gpui::px(radii.control());
    theme.radius_lg = gpui::px(radii.panel);
    // The base sizes every toolkit control inherits: 13 px sans and 12 px
    // mono, a step under the defaults, which is what the reference reads at.
    theme.font_size = gpui::px(13.);
    theme.mono_font_size = gpui::px(12.);

    // `Root` and several components paint from the derived semantic tokens
    // rather than from `colors`. Without regenerating them the window keeps
    // the toolkit's opaque default background, which cancels the glass.
    // The toolkit highlights fenced code in markdown with this, and it is
    // set once at startup otherwise: without it, a code block in a comment
    // keeps the light palette all the way through the dark theme.
    theme.highlight_theme = match mode {
        Mode::Dark => gpui_component::highlighter::HighlightTheme::default_dark(),
        Mode::Light => gpui_component::highlighter::HighlightTheme::default_light(),
    };

    theme.tokens = (&theme.colors).into();
    gpui_component::Theme::sync_base(cx);
}

impl Colors {
    /// A fully transparent fill, for surfaces that should show the window's
    /// glass rather than paint over it.
    fn transparent_surface(&self) -> Hsla {
        let mut color = self.bg_surface;
        color.a = 0.0;
        color
    }

    /// Whether these are the light theme's colours.
    fn light(&self) -> bool {
        self.appearance == ThemeAppearance::Light
    }

    /// A row under the pointer: the accent at a fraction of itself rather
    /// than a grey fill, because a grey fill over a translucent window is what
    /// turns glass into cardboard.
    ///
    /// The fraction is smaller on the light theme. A tint reads against a
    /// dark ground by adding light, which is gentle; against a light one it
    /// reads by adding colour, which at the same strength is a stain.
    pub fn row_hover(&self) -> Hsla {
        self.accent.opacity(if self.light() { 0.08 } else { 0.14 })
    }

    /// The row you are on.
    pub fn row_active(&self) -> Hsla {
        self.accent.opacity(if self.light() { 0.14 } else { 0.22 })
    }

    /// A popover's fill: the raised surface made opaque, because a menu
    /// that floats over text has to hide it — `Hsla::opacity` multiplies
    /// the alpha, so it cannot get there from a translucent token.
    pub fn popover(&self) -> Hsla {
        let mut color = self.bg_raised;
        color.a = 1.0;
        color
    }

    /// The merge button: the done colour deepened until white reads on it.
    /// The status green is made for a glyph on the glass, not for a fill
    /// under text, and white on it was the least legible thing on the page.
    pub fn merge_button(&self) -> Hsla {
        let mut color = self.status_done;
        color.l = 0.32;
        color.s = color.s.max(0.45);
        color.a = 1.0;
        color
    }

    /// A table's head: black over the glass, so it reads as a band and still
    /// lets the blur through. A third of it is a band on the dark theme and
    /// a slab on the light one, where the text over it is black too.
    pub fn table_head(&self) -> Hsla {
        gpui::black().opacity(if self.light() { 0.05 } else { 0.35 })
    }

    /// A raised control the pointer is over.
    pub fn surface_hover(&self) -> Hsla {
        self.bg_raised.opacity((self.bg_raised.a + 0.14).min(1.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_light_theme_tints_more_gently_than_the_dark_one() {
        let light = Tokens::load(Mode::Light);
        let dark = Tokens::load(Mode::Dark);
        assert_eq!(light.colors().appearance, ThemeAppearance::Light);
        assert_eq!(dark.colors().appearance, ThemeAppearance::Dark);
        assert!(light.colors().row_active().a < dark.colors().row_active().a);
        assert!(light.colors().row_hover().a < dark.colors().row_hover().a);
        // A band under black text cannot be a third of black.
        assert!(light.colors().table_head().a < 0.1);
    }

    #[test]
    fn both_built_in_themes_parse() {
        for mode in [Mode::Dark, Mode::Light] {
            let tokens = Tokens::load(mode);
            assert!(!tokens.name.is_empty());
            assert_eq!(tokens.radius.window, 12.0);
        }
    }

    #[test]
    fn appearance_matches_the_file_it_came_from() {
        assert_eq!(Tokens::load(Mode::Dark).appearance, ThemeAppearance::Dark);
        assert_eq!(Tokens::load(Mode::Light).appearance, ThemeAppearance::Light);
    }

    #[test]
    fn every_surface_lets_the_blur_through() {
        let dark = Tokens::load(Mode::Dark);
        for (name, colour) in [
            ("bg.window", dark.colors.bg_window),
            ("bg.sidebar", dark.colors.bg_sidebar),
            ("bg.surface", dark.colors.bg_surface),
            ("bg.raised", dark.colors.bg_raised),
        ] {
            assert!(colour.a < 1.0, "{name} must let the blur through");
        }
    }

    #[test]
    fn an_unmade_choice_takes_the_os_and_a_made_one_wins() {
        assert_eq!(Mode::resolve(None, WindowAppearance::Dark), Mode::Dark);
        assert_eq!(
            Mode::resolve(None, WindowAppearance::VibrantLight),
            Mode::Light
        );
        assert_eq!(
            Mode::resolve(Some(Appearance::Light), WindowAppearance::Dark),
            Mode::Light
        );
        assert_eq!(
            Mode::resolve(Some(Appearance::Dark), WindowAppearance::Light),
            Mode::Dark
        );
    }

    #[test]
    fn a_popover_is_opaque() {
        assert!(Tokens::load(Mode::Dark).colors.popover().a > 0.99);
    }

    #[test]
    fn the_merge_button_is_dark_enough_for_white_text() {
        for mode in [Mode::Dark, Mode::Light] {
            let fill = Tokens::load(mode).colors.merge_button();
            assert!(fill.l < 0.4, "white has to read on it");
            assert!(fill.a > 0.99, "a button is not glass");
        }
    }

    #[test]
    fn a_control_is_a_step_under_a_row_and_never_square() {
        let radii = Tokens::load(Mode::Dark).radius;
        assert!(radii.control() < radii.row);
        assert!(radii.control() >= 2.0);
    }

    #[test]
    fn the_sidebar_is_a_tint_over_the_glass_not_a_second_coat() {
        // Frost: the column reads lighter than the window behind it, and
        // thin enough that the desktop still shows through both.
        let dark = Tokens::load(Mode::Dark).colors;
        assert!(dark.bg_sidebar.a < 0.2);
        assert!(dark.bg_sidebar.l > dark.bg_window.l);
        assert!(dark.bg_window.a < 0.8);
    }

    #[test]
    fn the_logo_is_white_on_dark_and_navy_on_light() {
        assert!(Tokens::load(Mode::Dark).logo().l > 0.99);
        let navy = Tokens::load(Mode::Light).logo();
        assert!(navy.l < 0.25, "dark enough to read on the light glass");
        assert!(navy.s > 0.3, "blue, not grey");
    }

    #[test]
    fn a_table_head_is_a_dark_band_and_not_a_white_one() {
        let head = Tokens::load(Mode::Dark).colors.table_head();
        assert!(head.l < 0.1, "dark");
        assert!(head.a < 0.5, "and translucent");
    }

    #[test]
    fn hex_accepts_a_label_colour_without_its_hash() {
        assert!(parse_hex("d73a4a").is_ok());
        assert!(parse_hex("#FFF").is_err());
        assert!(parse_hex("#GGGGGG").is_err());
    }
}
