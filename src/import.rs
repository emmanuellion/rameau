//! "Importer un projet" page: open an existing repository, clone one, or create one.

use gpui::{ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px};
use gpui_component::{
    Disableable, IconName,
    button::ButtonVariants,
    input::Input,
};

use crate::app::{Page, Rameau};
use crate::theme::{c, mono, pal};
use crate::ui::{appear, button, button_sm};

fn card(step: &'static str, title: &'static str, text: impl Into<SharedString>) -> gpui::Div {
    let p = pal();
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap_2()
        .p_5()
        .rounded_lg()
        .border_1()
        .border_color(c(p.border))
        .bg(c(p.panel))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .size(px(22.))
                        .flex_none()
                        .rounded_full()
                        .bg(c(p.accent))
                        .text_color(c(p.bg))
                        .text_xs()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(step),
                )
                .child(div().text_base().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(p.text)).child(title)),
        )
        .child(div().text_sm().text_color(c(p.muted)).child(text.into()))
}

impl Rameau {
    pub fn import_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = pal();
        let busy = self.busy.is_some();
        let has_projects = !self.config.projects.is_empty();
        let shortcut = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };
        let dest = self.clone_parent.display().to_string();

        let page = div()
            .flex_none()
            .w_full()
            .max_w(px(680.))
            .mx_auto()
            .py_10()
            .px_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().flex_none().text_2xl().font_weight(gpui::FontWeight::BOLD).text_color(c(p.text)).child("Importer un projet git"))
            .child(div().flex_none().text_sm().text_color(c(p.muted)).child(
                "Rameau travaille directement sur les dépôts git de ton ordinateur. Importer un projet l'ajoute \
                 au sélecteur de projets (en haut à gauche) : aucun fichier n'est copié ni déplacé, et le retirer \
                 de la liste ne supprime rien sur le disque.",
            ))
            .child(appear(
                card(
                    "1",
                    "Ouvrir un dépôt existant",
                    "Choisis le dossier d'un projet déjà cloné : celui qui contient le dossier caché .git \
                     (un sous-dossier du projet marche aussi). Tu peux aussi glisser-déposer le dossier n'importe \
                     où dans cette fenêtre.",
                )
                .child(div().flex().child(
                    button("open-folder")
                        .icon(IconName::FolderOpen)
                        .label("Choisir un dossier…")
                        .primary()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.pick_folder(false, window, cx))),
                )),
                "card-1",
                10.,
                0.00,
            ))
            .child(appear(
                card(
                    "2",
                    "Cloner depuis une adresse",
                    "Colle l'adresse du dépôt : sur GitHub, bouton vert « Code », onglet HTTPS ou SSH. Pour un \
                     dépôt privé en HTTPS, connecte-toi d'abord avec GitHub (en bas de la barre latérale) ; en SSH, \
                     ce sont tes clés SSH habituelles qui servent.",
                )
                .child(Input::new(&self.clone_url))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .text_color(c(p.muted))
                        .child("Dans le dossier")
                        .child(div().min_w_0().truncate().font_family(mono()).text_color(c(p.text)).child(dest))
                        .child(
                            button_sm("clone-parent")
                                .label("Changer…")
                                .ghost()
                                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.pick_clone_parent(window, cx))),
                        ),
                )
                .child(div().flex().child(
                    button("clone")
                        .label("Cloner")
                        .primary()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.clone_repo(window, cx))),
                )),
                "card-2",
                10.,
                0.12,
            ))
            .child(appear(
                card(
                    "3",
                    "Créer un nouveau dépôt",
                    "Choisis un dossier (vide ou contenant déjà ton code) : Rameau y lance git init, puis tu fais \
                     ton premier commit depuis « Modifications locales ».",
                )
                .child(div().flex().child(
                    button("init-folder")
                        .label("Choisir un dossier…")
                        .outline()
                        .disabled(busy)
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.pick_folder(true, window, cx))),
                )),
                "card-3",
                10.,
                0.24,
            ))
            .child(div().flex_none().text_xs().text_color(c(p.faint)).child(format!(
                "Pour changer de projet ensuite : clique sur son nom en haut à gauche. Depuis un terminal : rameau /chemin/du/projet. \
                 Raccourcis : ↑ ↓ pour parcourir l'historique, Échap pour fermer un diff, {shortcut}↩ pour valider un commit."
            )))
            .when(has_projects && self.proj.is_some(), |el| {
                el.child(
                    button("back")
                        .icon(IconName::ArrowLeft)
                        .label("Retour au projet")
                        .ghost()
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.page = Page::Repo;
                            cx.notify();
                        })),
                )
            });
        div().id("import-page").flex_1().min_w_0().min_h_0().flex().flex_col().overflow_y_scroll().child(page)
    }
}
