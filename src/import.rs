//! "Importer un projet" page: open an existing repository, clone one, or create one.

use gpui::{AnyElement, ClickEvent, Context, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px, uniform_list};
use gpui_component::{
    Disableable, Icon, IconName, Sizable,
    button::ButtonVariants,
    input::Input,
};

use crate::app::{Login, Page, Rameau};
use crate::github::RemoteRepo;
use crate::theme::{c, mono, pal};
use crate::ui::{appear, button, button_sm, icon};

/// Height of a repository line: the list is virtualized.
const REPO_ROW_H: f32 = 52.;

/// Repositories whose name or description contains every word of the search, in any case.
pub fn matching<'a>(repos: &'a [RemoteRepo], search: &str) -> Vec<&'a RemoteRepo> {
    let words: Vec<String> = search.split_whitespace().map(str::to_lowercase).collect();
    repos
        .iter()
        .filter(|r| {
            let text = format!("{} {}", r.full_name, r.description).to_lowercase();
            words.iter().all(|w| text.contains(w.as_str()))
        })
        .collect()
}

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
    /// Card listing the GitHub account's repositories, each ready to clone in one click.
    fn github_card(&self, cx: &mut Context<Self>) -> gpui::Div {
        let p = pal();
        let busy = self.busy.is_some();
        let card = card(
            "1",
            "Cloner un de tes dépôts GitHub",
            "Tes dépôts, ceux où tu collabores et ceux de tes organisations, les plus récents d'abord. \
             « Cloner » télécharge le projet dans le dossier choisi plus bas, puis l'ouvre.",
        );
        let body = match (&self.login, &self.gh_repos) {
            (Login::Out, _) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    button("gh-login")
                        .primary()
                        .icon(IconName::GitHub)
                        .label("Se connecter avec GitHub")
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.login(window, cx))),
                )
                .child(div().text_xs().text_color(c(p.faint)).child("La liste apparaît ici une fois connecté."))
                .into_any_element(),
            (Login::Waiting(code), _) => div()
                .text_sm()
                .text_color(c(p.muted))
                .child(format!("Colle le code {code} sur la page GitHub ouverte dans ton navigateur…"))
                .into_any_element(),
            (Login::In(_), None) => div().text_sm().text_color(c(p.muted)).child("Chargement de tes dépôts…").into_any_element(),
            (Login::In(_), Some(Err(e))) => div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().flex_1().text_sm().text_color(c(p.red)).child(e.clone()))
                .child(button_sm("gh-retry").outline().label("Réessayer").on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.fetch_repos(cx))))
                .into_any_element(),
            (Login::In(_), Some(Ok(repos))) => {
                let search = self.repo_filter.read(cx).value().to_string();
                let shown = matching(repos, &search).len();
                let list = uniform_list(
                    "gh-repos",
                    shown,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let Some(Ok(repos)) = &this.gh_repos else { return vec![] };
                        let search = this.repo_filter.read(cx).value().to_string();
                        let found = matching(repos, &search);
                        range.filter_map(|i| found.get(i).map(|r| repo_row(i, r, busy, cx))).collect::<Vec<_>>()
                    }),
                )
                .h(px((shown.clamp(1, 6) as f32) * REPO_ROW_H));
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().child(Input::new(&self.repo_filter).prefix(Icon::new(IconName::Search).small().text_color(c(p.faint)))))
                            .child(
                                button_sm("gh-refresh")
                                    .ghost()
                                    .icon(icon("refresh-cw"))
                                    .tooltip("Actualiser la liste")
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.fetch_repos(cx))),
                            ),
                    )
                    .child(div().text_xs().text_color(c(p.faint)).child(match (repos.len(), shown) {
                        (0, _) => "Aucun dépôt sur ce compte.".to_owned(),
                        (n, s) if s == n => format!("{n} dépôt{}", if n > 1 { "s" } else { "" }),
                        (n, s) => format!("{s} sur {n}"),
                    }))
                    .child(div().rounded(px(8.)).border_1().border_color(c(p.border)).bg(c(p.bg)).child(list))
                    .into_any_element()
            }
        };
        card.child(body)
    }

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
            .child(appear(self.github_card(cx), "card-gh", 10., 0.))
            .child(appear(
                card(
                    "2",
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
                0.08,
            ))
            .child(appear(
                card(
                    "3",
                    "Cloner depuis une adresse",
                    "Colle l'adresse du dépôt : sur GitHub, bouton vert « Code », onglet HTTPS ou SSH. Pour un \
                     dépôt privé en HTTPS, connecte-toi d'abord avec GitHub (en haut à droite) ; en SSH, \
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
                0.16,
            ))
            .child(appear(
                card(
                    "4",
                    "Créer un nouveau dépôt",
                    "Choisis un dossier (vide ou contenant déjà ton code) : Rameau y lance git init, puis tu fais \
                     ton premier commit depuis « Modifications ».",
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

fn repo_row(i: usize, r: &RemoteRepo, busy: bool, cx: &mut Context<Rameau>) -> AnyElement {
    let p = pal();
    let url = r.clone_url.clone();
    div()
        .id(("gh-repo", i))
        .h(px(REPO_ROW_H))
        .w_full()
        .flex()
        .items_center()
        .gap_3()
        .px_3()
        .border_b_1()
        .border_color(c(p.border))
        .hover(|s| s.bg(c(p.hover)))
        .child(if r.private { icon("lock") } else { Icon::new(IconName::Globe) }.small().text_color(c(p.faint)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().truncate().text_sm().font_weight(gpui::FontWeight::MEDIUM).text_color(c(p.text)).child(r.full_name.clone()))
                .when(!r.description.is_empty(), |d| d.child(div().truncate().text_xs().text_color(c(p.muted)).child(r.description.clone()))),
        )
        .child(
            button_sm(("gh-clone", i))
                .outline()
                .label("Cloner")
                .disabled(busy)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| this.clone_from(url.clone(), window, cx))),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_every_word_in_any_case() {
        let repo = |name: &str, description: &str| RemoteRepo { full_name: name.into(), clone_url: String::new(), description: description.into(), private: false };
        let repos = [repo("ada/rameau", "Client git en Rust"), repo("acme/site", "Site vitrine"), repo("ada/notes", "")];
        let names = |s: &str| matching(&repos, s).iter().map(|r| r.full_name.clone()).collect::<Vec<_>>();
        assert_eq!(names(""), ["ada/rameau", "acme/site", "ada/notes"]);
        assert_eq!(names("ADA"), ["ada/rameau", "ada/notes"]);
        assert_eq!(names("ada rust"), ["ada/rameau"], "words can come from the description");
        assert!(names("inconnu").is_empty());
    }
}
