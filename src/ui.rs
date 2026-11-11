//! Shared building blocks: buttons with a hand cursor and room to breathe, Rameau's own icons,
//! avatars and dialog footers.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt, App, AssetSource, ElementId, IntoElement, ParentElement, Result, SharedString,
    Styled, Window, div, ease_out_quint, px,
};
use gpui_component::{
    Icon, Sizable, WindowExt,
    button::{Button, ButtonVariant, ButtonVariants},
    dialog::Dialog,
};

use crate::theme::{alpha, c, pal};

/// Icons gpui-component doesn't ship (Lucide, ISC license), then gpui-component's own.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let ours: Option<&'static [u8]> = match path {
            "icons/rameau/git-branch.svg" => Some(include_bytes!("../assets/icons/git-branch.svg")),
            "icons/rameau/git-commit.svg" => Some(include_bytes!("../assets/icons/git-commit.svg")),
            "icons/rameau/refresh-cw.svg" => Some(include_bytes!("../assets/icons/refresh-cw.svg")),
            "icons/rameau/cloud.svg" => Some(include_bytes!("../assets/icons/cloud.svg")),
            "icons/rameau/archive.svg" => Some(include_bytes!("../assets/icons/archive.svg")),
            _ => None,
        };
        match ours {
            Some(bytes) => Ok(Some(Cow::Borrowed(bytes))),
            None => gpui_component_assets::Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

/// One of Rameau's icons: `git-branch`, `git-commit`, `refresh-cw`, `cloud`, `archive`.
pub fn icon(name: &str) -> Icon {
    Icon::empty().path(format!("icons/rameau/{name}.svg"))
}

/// Standard button: hand cursor, 32px tall, comfortable padding.
/// No text style here: a text refinement on gpui-component buttons drops their text color.
pub fn button(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer().h(px(32.)).px_3p5().gap_2().rounded(px(8.))
}

/// Smaller button for dense places (section headers, toolbars inside panels).
pub fn button_sm(id: impl Into<ElementId>) -> Button {
    Button::new(id).cursor_pointer().small().h(px(28.)).px_2p5().gap_1p5().rounded(px(7.))
}

/// Square icon-only button.
pub fn icon_button(id: impl Into<ElementId>, icon: impl Into<Icon>) -> Button {
    Button::new(id).cursor_pointer().icon(icon).ghost().small().size(px(28.)).rounded(px(7.))
}

/// Round avatar with initials, colored from the name so a person keeps the same color.
pub fn avatar(name: &str, size: f32) -> impl IntoElement + use<> {
    let p = pal();
    let initials: String = name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
    let color = p.lanes[crate::graph::color_index(name.as_bytes()) % p.lanes.len()];
    div()
        .size(px(size))
        .flex_none()
        .rounded_full()
        .bg(alpha(color, 0.22))
        .text_color(c(color))
        .text_size(px(size * 0.42))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .child(if initials.is_empty() { "?".to_owned() } else { initials })
}

/// Cancel + OK footer built from our buttons (hand cursor, our sizes), running `on_ok`;
/// the dialog closes when it returns true. Enter in the dialog runs it too.
pub fn with_footer(
    dialog: Dialog,
    ok: &'static str,
    danger: bool,
    on_ok: impl Fn(&mut Window, &mut App) -> bool + 'static,
) -> Dialog {
    let on_ok = Rc::new(on_ok);
    let enter = on_ok.clone();
    dialog
        .on_ok(move |_, window, cx| enter(window, cx))
        .footer(move |_, _, _, _| {
            let on_ok = on_ok.clone();
            vec![
                button("dialog-cancel")
                    .label("Annuler")
                    .ghost()
                    .on_click(|_, window, cx| window.close_dialog(cx))
                    .into_any_element(),
                button("dialog-ok")
                    .label(ok)
                    .with_variant(if danger { ButtonVariant::Danger } else { ButtonVariant::Primary })
                    .on_click(move |_, window, cx| {
                        if on_ok(window, cx) {
                            window.close_dialog(cx);
                        }
                    })
                    .into_any_element(),
            ]
        })
        .overlay_closable(false)
}

/// Stable number for an animation id, so the animation replays only when `value` changes.
pub fn key(value: impl Hash) -> usize {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut h);
    h.finish() as usize
}

/// Fades in while rising by `rise` px, in 180ms. `delay` (0..1) holds it back for a
/// fraction of the time, to stagger a few items. Plays once per `id`.
pub fn appear<E: Styled + IntoElement + 'static>(el: E, id: impl Into<ElementId>, rise: f32, delay: f32) -> AnimationElement<E> {
    let ease = ease_out_quint();
    let ms = (180. / (1. - delay)) as u64;
    el.with_animation(
        id,
        Animation::new(Duration::from_millis(ms)).with_easing(move |t| ease(((t - delay) / (1. - delay)).clamp(0., 1.))),
        move |el, t| el.opacity(t).mt(px(rise * (1. - t))),
    )
}
