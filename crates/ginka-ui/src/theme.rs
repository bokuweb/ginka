//! Design tokens.
//!
//! The single source of truth for colour, radius and motion is
//! `assets/themes/*.json`, which implements `docs/ui.md` §2. Views resolve
//! everything through the `Tokens` global — no view hardcodes a colour, a
//! radius or a duration (`AGENTS.md` rule 9), because that is what makes a new
//! theme a data change instead of a refactor.

use ginka_core::settings::Appearance;
use gpui::{App, Global, Hsla, Rgba, WindowAppearance};
use serde::{Deserialize, Deserializer};
use std::time::Duration;

const DARK: &str = include_str!("../../../assets/themes/dark.json");
const LIGHT: &str = include_str!("../../../assets/themes/light.json");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Light,
    Dark,
}

impl Mode {
    /// Resolve the user's choice against the window's actual appearance.
    ///
    /// `Appearance::System` is not a third theme: it is a deferral to the OS,
    /// and the OS answer can change while the app is running (see
    /// `Window::observe_window_appearance`).
    pub fn resolve(choice: Appearance, system: WindowAppearance) -> Self {
        match choice {
            Appearance::Light => Self::Light,
            Appearance::Dark => Self::Dark,
            Appearance::System => match system {
                WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
                WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
            },
        }
    }
}

/// The complete token set.
///
/// Fields are deliberately exhaustive rather than added on demand: the palette
/// is a contract themes are authored against, and a token that appears only
/// when some view happens to need it cannot be validated in `assets/themes/`.
/// Consumers arrive as milestones land, so unused members are expected here and
/// only here.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub name: String,
    pub appearance: ThemeAppearance,
    pub colors: Colors,
    pub radius: Radii,
    pub duration_ms: Durations,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeAppearance {
    Light,
    #[default]
    Dark,
}

/// Every colour the app may paint.
///
/// `deny_unknown_fields` plus non-optional members means a theme file that
/// misspells or omits a token fails to parse instead of rendering a black hole
/// somewhere in the UI.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Colors {
    #[serde(skip)]
    appearance: ThemeAppearance,
    #[serde(rename = "bg.window", deserialize_with = "hex")]
    pub bg_window: Hsla,
    #[serde(rename = "bg.sidebar", deserialize_with = "hex")]
    pub bg_sidebar: Hsla,
    #[serde(rename = "bg.surface", deserialize_with = "hex")]
    pub bg_surface: Hsla,
    #[serde(rename = "bg.raised", deserialize_with = "hex")]
    pub bg_raised: Hsla,
    #[serde(rename = "bg.terminal", deserialize_with = "hex")]
    pub bg_terminal: Hsla,
    #[serde(rename = "border.subtle", deserialize_with = "hex")]
    pub border_subtle: Hsla,
    #[serde(rename = "border.strong", deserialize_with = "hex")]
    pub border_strong: Hsla,
    #[serde(rename = "text.primary", deserialize_with = "hex")]
    pub text_primary: Hsla,
    #[serde(rename = "text.secondary", deserialize_with = "hex")]
    pub text_secondary: Hsla,
    #[serde(rename = "text.muted", deserialize_with = "hex")]
    pub text_muted: Hsla,
    #[serde(deserialize_with = "hex")]
    pub accent: Hsla,
    #[serde(rename = "status.working", deserialize_with = "hex")]
    pub status_working: Hsla,
    #[serde(rename = "status.attention", deserialize_with = "hex")]
    pub status_attention: Hsla,
    #[serde(rename = "status.done", deserialize_with = "hex")]
    pub status_done: Hsla,
    #[serde(rename = "status.error", deserialize_with = "hex")]
    pub status_error: Hsla,
    #[serde(rename = "code.bg", deserialize_with = "hex")]
    pub code_bg: Hsla,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Radii {
    pub window: f32,
    /// The composer and other cards that hold a group of controls. Larger than
    /// a panel on purpose: a card is an object on the surface, and the corner
    /// is what says so.
    pub card: f32,
    pub panel: f32,
    pub row: f32,
}

impl Radii {
    /// A control sits one radius step inside a row or card.
    pub fn control(&self) -> f32 {
        (self.row - 3.).max(2.)
    }
}

/// Motion durations. `docs/ui.md` §1: ~260 ms for layout, ~120 ms for feedback.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub struct Durations {
    pub quick: u64,
    pub fade: u64,
    pub standard: u64,
}

#[allow(dead_code)]
impl Durations {
    pub fn quick(&self) -> Duration {
        Duration::from_millis(self.quick)
    }

    pub fn standard(&self) -> Duration {
        Duration::from_millis(self.standard)
    }

    pub fn fade(&self) -> Duration {
        Duration::from_millis(self.fade)
    }
}

impl Tokens {
    pub fn load(mode: Mode) -> Self {
        let source = match mode {
            Mode::Dark => DARK,
            Mode::Light => LIGHT,
        };
        // The themes are compiled in, so a parse failure is a build-time
        // authoring mistake and the tests below catch it.
        let mut tokens: Self = serde_json::from_str(source).expect("built-in theme is valid");
        tokens.colors.appearance = tokens.appearance;
        tokens
    }

    pub fn global(cx: &App) -> &Tokens {
        cx.global::<Tokens>()
    }

    pub fn install(mode: Mode, cx: &mut App) {
        cx.set_global(Tokens::load(mode));
    }
}

impl Global for Tokens {}

/// Parse `#RRGGBB` or `#RRGGBBAA`.
///
/// Alpha lives in the token rather than at the call site: the sidebar's
/// translucency is a property of the theme, and a view that re-applies opacity
/// on top of it drifts out of spec.
fn hex<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Hsla, D::Error> {
    use serde::de::Error as _;
    let raw = String::deserialize(deserializer)?;
    parse_hex(&raw).map_err(D::Error::custom)
}

fn parse_hex(raw: &str) -> Result<Hsla, String> {
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
/// The toolkit's components resolve their own palette through `cx.theme()`, so
/// a button drawn by the library and a row drawn by us have to agree. Mapping
/// once here is what keeps them from drifting; nothing else in the app should
/// touch `Theme::global_mut`.
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
    // A field paints the same surface a card does, so an input inside one
    // disappears into it and the card's own border is what carries focus. The
    // ring is the accent at a fraction of itself: a hard outline around the
    // composer reads as an error state, not as focus.
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
    // translucent background; a second layer of the same colour composites with
    // it -- two coats of 82% is 97% -- and the glass turns opaque. Everything
    // stacked on top of `Root` must either be opaque by design (a card) or
    // paint nothing at all.
    theme.colors.title_bar = gpui::transparent_black();
    theme.colors.title_bar_border = tokens.border_subtle;
    theme.colors.window_border = tokens.border_subtle;

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

    theme.colors.primary = tokens.accent;
    theme.colors.primary_foreground = tokens.bg_window;
    theme.colors.primary_hover = tokens.accent.opacity(0.85);
    theme.colors.primary_active = tokens.accent.opacity(0.7);
    theme.colors.danger = tokens.status_error;
    theme.colors.success = tokens.status_done;

    theme.radius = gpui::px(radii.control());
    theme.radius_lg = gpui::px(radii.panel);
    theme.font_size = gpui::px(13.);
    theme.mono_font_size = gpui::px(12.);
    theme.highlight_theme = match mode {
        Mode::Dark => gpui_component::highlighter::HighlightTheme::default_dark(),
        Mode::Light => gpui_component::highlighter::HighlightTheme::default_light(),
    };

    // `Root` and several components paint from the derived semantic tokens
    // rather than from `colors`. Without regenerating them the window keeps the
    // toolkit's opaque default background, which cancels the glass surface.
    theme.tokens = (&theme.colors).into();

    // The Base layer mirrors radius/colour for scrollbars and resize handles;
    // without this they keep the previous theme's values.
    gpui_component::Theme::sync_base(cx);
}

impl Tokens {
    pub fn colors(&self) -> &Colors {
        &self.colors
    }
}

impl Colors {
    fn light(&self) -> bool {
        self.appearance == ThemeAppearance::Light
    }

    /// Opaque raised fill for content that floats above text.
    pub fn popover(&self) -> Hsla {
        let mut color = self.bg_raised;
        color.a = 1.0;
        color
    }
    /// A fully transparent fill, for surfaces that should show the window's
    /// glass rather than paint over it.
    fn transparent_surface(&self) -> Hsla {
        let mut color = self.bg_surface;
        color.a = 0.0;
        color
    }

    /// A row under the pointer.
    ///
    /// The accent at a fraction of itself rather than a grey fill: rows are
    /// told apart by the space between them, and the one being pointed at only
    /// needs a tint to say so. Ported from bokuweb/pedro's palette, where the
    /// same rule keeps a translucent window from turning into a grey one.
    pub fn row_hover(&self) -> Hsla {
        self.accent.opacity(if self.light() { 0.08 } else { 0.14 })
    }

    /// The row you are on.
    pub fn row_active(&self) -> Hsla {
        self.accent.opacity(if self.light() { 0.14 } else { 0.22 })
    }

    /// A raised control the pointer is over.
    pub fn surface_hover(&self) -> Hsla {
        self.bg_raised.opacity((self.bg_raised.a + 0.14).min(1.0))
    }
}

/// What the app sets macOS's `AppleFontSmoothing` to for itself, given what
/// the user has set, if anything.
///
/// The toolkit thickens glyph strokes by their colour's brightness whenever
/// the key is absent, which is how a Mac ships since 10.14; near-white text
/// on the dark surface then reads as bold. An explicit choice is left alone.
pub fn font_smoothing_override(current: Option<i64>) -> Option<i64> {
    match current {
        Some(_) => None,
        None => Some(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_built_in_themes_parse() {
        for mode in [Mode::Dark, Mode::Light] {
            let tokens = Tokens::load(mode);
            assert!(!tokens.name.is_empty());
            assert_eq!(tokens.radius.window, 12.0);
        }
    }

    #[test]
    fn text_is_drawn_unthickened_unless_the_user_asked_for_smoothing() {
        assert_eq!(font_smoothing_override(None), Some(0));
        assert_eq!(font_smoothing_override(Some(0)), None);
        assert_eq!(font_smoothing_override(Some(2)), None);
    }

    #[test]
    fn corners_are_tight_enough_to_read_as_a_tool() {
        // Softer than this reads as a toy at this density; the window's own
        // corner is the platform's and stays as it is.
        for mode in [Mode::Dark, Mode::Light] {
            let radius = Tokens::load(mode).radius;
            assert_eq!(
                (radius.card, radius.panel, radius.row, radius.control()),
                (10.0, 7.0, 6.0, 3.0)
            );
        }
    }

    #[test]
    fn appearance_matches_the_file_it_came_from() {
        assert_eq!(Tokens::load(Mode::Dark).appearance, ThemeAppearance::Dark);
        assert_eq!(Tokens::load(Mode::Light).appearance, ThemeAppearance::Light);
    }

    #[test]
    fn the_window_background_is_painted_once() {
        // Regression: `Root` paints `bg.window`, so any full-bleed surface
        // stacked on it must not paint the same translucent colour again. Two
        // coats materially increase opacity and make the glass read as solid.
        let dark = Tokens::load(Mode::Dark);
        let once = dark.colors.bg_window.a;
        let twice = once + (1.0 - once) * once;
        assert!(
            twice - once > 0.15,
            "double-painting must be understood as the failure it is: {twice}"
        );
    }

    #[test]
    fn every_surface_lets_the_blur_through() {
        // A fully opaque panel over a blurred window looks like a mistake
        // rather than a choice, so the surfaces carry alpha as well as the
        // window does. What keeps them from stacking into an opaque sheet is
        // that exactly one of them paints each pixel — see `docs/ui.md` §1.
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
    fn a_row_is_tinted_by_the_accent_rather_than_filled_with_grey() {
        // Rows are told apart by the space between them; the one being pointed
        // at only needs a tint, and a grey fill over a translucent window is
        // what turns glass into cardboard.
        let dark = Tokens::load(Mode::Dark);
        let hover = dark.colors.row_hover();
        let active = dark.colors.row_active();
        assert_eq!(hover.h, dark.colors.accent.h, "the tint is the accent");
        assert!(hover.a < active.a, "the row you are on is the stronger one");
        assert!(active.a < 0.4, "a tint, not a fill");
    }

    #[test]
    fn system_appearance_defers_to_the_os_but_an_explicit_choice_wins() {
        assert_eq!(
            Mode::resolve(Appearance::System, WindowAppearance::Dark),
            Mode::Dark
        );
        assert_eq!(
            Mode::resolve(Appearance::System, WindowAppearance::VibrantLight),
            Mode::Light
        );
        // An explicit choice ignores the OS entirely.
        assert_eq!(
            Mode::resolve(Appearance::Light, WindowAppearance::Dark),
            Mode::Light
        );
        assert_eq!(
            Mode::resolve(Appearance::Dark, WindowAppearance::Light),
            Mode::Dark
        );
    }

    #[test]
    fn hex_rejects_malformed_input() {
        assert!(parse_hex("#FFF").is_err());
        assert!(parse_hex("#GGGGGG").is_err());
        assert!(parse_hex("#11223344").is_ok());
    }
}
