//! Rameau's colors, in a dark and a light variant.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::{App, Rgba, Window, rgb};
use gpui_component::{Theme, ThemeMode};

pub struct Palette {
    pub bg: u32,
    pub panel: u32,
    pub border: u32,
    pub hover: u32,
    pub selected: u32,
    pub text: u32,
    pub muted: u32,
    pub faint: u32,
    pub accent: u32,
    pub green: u32,
    pub red: u32,
    pub yellow: u32,
    pub lanes: [u32; 10],
}

const DARK: Palette = Palette {
    bg: 0x14161b,
    panel: 0x1a1d23,
    border: 0x262a33,
    hover: 0x1f232b,
    selected: 0x262d3b,
    text: 0xe4e7ee,
    muted: 0x8a90a0,
    faint: 0x5c6270,
    accent: 0x5ab0f6,
    green: 0x7bd88f,
    red: 0xef6461,
    yellow: 0xf2d35b,
    lanes: [0x5ab0f6, 0xf0719b, 0x7bd88f, 0xf5a65b, 0xb18cf5, 0x4fd1c5, 0xf2d35b, 0xef6461, 0x8fa8ff, 0xc3e66e],
};

// Same hues, darker so lines and text stay readable on white.
const LIGHT: Palette = Palette {
    bg: 0xffffff,
    panel: 0xf6f7f9,
    border: 0xe3e5ea,
    hover: 0xf0f2f5,
    selected: 0xe2ebfa,
    text: 0x1d2129,
    muted: 0x5f6673,
    faint: 0x9aa0ac,
    accent: 0x2f7fd1,
    green: 0x2e9e4f,
    red: 0xd64541,
    yellow: 0xb8860b,
    lanes: [0x2f7fd1, 0xd6457a, 0x2e9e4f, 0xd9822b, 0x8a5cd6, 0x1f9e93, 0xb8921a, 0xd64541, 0x5470d6, 0x6f9a1c],
};

static LIGHT_MODE: AtomicBool = AtomicBool::new(false);

pub fn pal() -> &'static Palette {
    if is_light() { &LIGHT } else { &DARK }
}

pub fn is_light() -> bool {
    LIGHT_MODE.load(Ordering::Relaxed)
}

pub fn c(color: u32) -> Rgba {
    rgb(color)
}

pub fn alpha(color: u32, a: f32) -> Rgba {
    Rgba { a, ..rgb(color) }
}

pub fn lane_color(i: usize) -> Rgba {
    let lanes = &pal().lanes;
    rgb(lanes[i % lanes.len()])
}

pub fn status_color(status: char) -> Rgba {
    let p = pal();
    rgb(match status {
        'A' => p.green,
        'D' | 'U' => p.red,
        'R' => p.accent,
        _ => p.yellow,
    })
}

pub fn mono() -> &'static str {
    if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    }
}

/// Switches the palette and restyles the components (buttons, inputs, menus) to match.
pub fn apply(light: bool, window: Option<&mut Window>, cx: &mut App) {
    LIGHT_MODE.store(light, Ordering::Relaxed);
    Theme::change(if light { ThemeMode::Light } else { ThemeMode::Dark }, window, cx);
    let p = pal();
    let theme = Theme::global_mut(cx);
    theme.primary = rgb(p.accent).into();
    theme.primary_hover = alpha(p.accent, 0.85).into();
    theme.primary_active = alpha(p.accent, 0.7).into();
    theme.primary_foreground = rgb(p.bg).into();
    theme.ring = rgb(p.accent).into();
    theme.background = rgb(p.bg).into();
    theme.popover = rgb(p.panel).into();
    theme.border = rgb(p.border).into();
    theme.input = rgb(p.border).into();
    theme.title_bar = rgb(p.panel).into();
    theme.title_bar_border = rgb(p.border).into();
    theme.foreground = rgb(p.text).into();
    theme.muted_foreground = rgb(p.muted).into();
    theme.mono_font_family = mono().into();
}
