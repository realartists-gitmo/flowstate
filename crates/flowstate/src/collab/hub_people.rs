//! CO-S2: the Hub's People and Discovery rooms. Identity card (P5-S5),
//! the trust ceremony, squads, and discovery controls — migrated here from
//! Settings, because trust is a social act, not configuration. CO-S3: trust
//! grants expose their SCOPE with a picker and a legend.

use gpui::{AnyElement, Context, IntoElement, ParentElement, SharedString, Window, div, prelude::*, px, rgb};
use gpui_component::{
  ActiveTheme as _, Sizable as _, StyledExt as _,
  button::{Button, ButtonVariants as _},
  clipboard::Clipboard,
  h_flex,
  menu::{DropdownMenu as _, PopupMenuItem},
  v_flex,
};

use super::CollabShareDialog;
use super::share_dialog_view::section_title;

#[hotpath::measure_all]
impl CollabShareDialog {
  pub(super) fn render_people_room(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
    let workspace = self.workspace.clone();
    v_flex()
      .gap_4()
      .child(self.render_identity_card(cx))
      .child(crate::workspace::render_collaboration_profile(workspace.clone(), window, cx))
      .child(section_title("Trusted people", cx))
      .child(scope_legend(cx))
      .child(crate::workspace::render_trusted_collaborators(workspace.clone(), window, cx))
      .child(self.render_scope_pickers(cx))
      .child(section_title("Squads", cx))
      .child(crate::workspace::render_collaboration_squads(workspace, window, cx))
      .into_any_element()
  }

  pub(super) fn render_discovery_room(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
    let workspace = self.workspace.clone();
    v_flex()
      .gap_4()
      .child(section_title("Nearby trusted peers", cx))
      .child(self.discovery_panel(cx))
      .child(crate::workspace::render_collaboration_discovery_pause(workspace.clone(), window, cx))
      .child(crate::workspace::render_collaboration_bluetooth(workspace, window, cx))
      .into_any_element()
  }

  /// P5-S5: who you are on the wire — name, palette-constrained color, your
  /// portable identity key (the thing others paste to trust you).
  fn render_identity_card(&self, cx: &mut Context<Self>) -> AnyElement {
    let profile = crate::app_settings::load_local_user_profile();
    let identity_key: Option<SharedString> = crate::app_settings::load_local_identity_secret()
      .map(|secret| secret.public().to_string().into());
    let initial: SharedString = profile
      .display_name
      .chars()
      .next()
      .map_or_else(|| "?".into(), |ch| ch.to_uppercase().to_string().into());

    v_flex()
      .gap_2()
      .p_3()
      .rounded_md()
      .border_1()
      .border_color(cx.theme().border)
      .child(section_title("You", cx))
      .child(
        h_flex()
          .gap_3()
          .items_center()
          .child(
            div()
              .w(px(40.0))
              .h(px(40.0))
              .rounded_full()
              .flex()
              .items_center()
              .justify_center()
              .bg(gpui::Hsla::from(rgb(profile.color_rgb)))
              .text_color(gpui::white())
              .font_weight(gpui::FontWeight::SEMIBOLD)
              .child(initial),
          )
          .child(
            v_flex()
              .gap_0p5()
              .child(div().text_sm().font_semibold().child(SharedString::from(profile.display_name.clone())))
              .child(
                div()
                  .text_xs()
                  .text_color(cx.theme().muted_foreground)
                  .child("Your presence color — collaborators see it on carets, comments, and blame."),
              ),
          ),
      )
      // Palette-constrained color picker (one id, one rendering — the same
      // eight colors presence uses).
      .child(h_flex().gap_1().children(flowstate_collab::ids::PALETTE.iter().enumerate().map(|(ix, color)| {
        let color = *color;
        let selected = profile.color_rgb == color;
        let name = profile.display_name.clone();
        let avatar = profile.avatar_path.clone();
        div()
          .id(("identity-color", ix))
          .w(px(22.0))
          .h(px(22.0))
          .rounded_full()
          .bg(gpui::Hsla::from(rgb(color)))
          .cursor_pointer()
          .when(selected, |this| this.border_2().border_color(cx.theme().foreground))
          .on_mouse_down(
            gpui::MouseButton::Left,
            cx.listener(move |_, _, _, cx| {
              if let Err(error) = crate::app_settings::save_local_collaboration_profile(name.clone(), color, avatar.clone()) {
                tracing::warn!(%error, "saving presence color failed");
              }
              cx.notify();
            }),
          )
      })))
      .when_some(identity_key, |this, key| {
        this.child(
          h_flex()
            .gap_2()
            .items_center()
            .child(
              div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .overflow_hidden()
                .text_ellipsis()
                .text_color(cx.theme().muted_foreground)
                .child(key.clone()),
            )
            .child(Clipboard::new("identity-key-copy").value(key.clone())),
        )
      })
      .child(
        div()
          .text_xs()
          .text_color(cx.theme().muted_foreground)
          .child("Share this key with someone who wants to trust you; you'll each see the same safety code."),
      )
      .into_any_element()
  }

  /// CO-S3: per-person scope chips + a picker to add one. Scopes live on the
  /// same records the standing-access check reads — this is the whole law.
  fn render_scope_pickers(&self, cx: &mut Context<Self>) -> AnyElement {
    let settings = crate::app_settings::load_app_settings();
    if settings.trusted_collaborators.is_empty() {
      return div().into_any_element();
    }
    let active_path = self
      .workspace
      .upgrade()
      .and_then(|workspace| workspace.read(cx).active_document_path(cx));
    v_flex()
      .gap_2()
      .children(settings.trusted_collaborators.into_iter().enumerate().map(|(person_ix, person)| {
        let identity_key = person.identity_key.clone();
        v_flex()
          .gap_1()
          .child(
            div()
              .text_xs()
              .text_color(cx.theme().muted_foreground)
              .child(format!("{} can reach:", person.display_name)),
          )
          .child(
            h_flex()
              .gap_1()
              .flex_wrap()
              .children(person.scopes.iter().enumerate().map(|(scope_ix, scope)| {
                let label: SharedString = scope_label(scope).into();
                let remove_key = identity_key.clone();
                h_flex()
                  .gap_1()
                  .items_center()
                  .px_1()
                  .rounded_sm()
                  .border_1()
                  .border_color(cx.theme().border)
                  .child(div().text_xs().child(label))
                  .child(
                    Button::new(("scope-remove", person_ix * 100 + scope_ix))
                      .text()
                      .compact()
                      .child(div().text_xs().text_color(cx.theme().muted_foreground).child("×"))
                      .on_click(cx.listener(move |_, _, _, cx| {
                        remove_collaborator_scope(&remove_key, scope_ix);
                        cx.notify();
                      })),
                  )
                  .into_any_element()
              }))
              .child({
                let add_key = identity_key.clone();
                let document_path = active_path.clone();
                Button::new(("scope-add", person_ix))
                  .xsmall()
                  .ghost()
                  .label("+ scope")
                  .dropdown_menu(move |menu, _, _| {
                    let doc_key = add_key.clone();
                    let folder_key = add_key.clone();
                    let global_key = add_key.clone();
                    let doc_path = document_path.clone();
                    let folder_path = document_path.as_ref().and_then(|path| path.parent().map(std::path::Path::to_path_buf));
                    menu
                      .item(PopupMenuItem::new("This document").on_click(move |_, _, cx| {
                        if let Some(path) = doc_path.clone() {
                          add_collaborator_scope(&doc_key, crate::app_settings::CollaborationScope::Document(path));
                          cx.refresh_windows();
                        }
                      }))
                      .item(PopupMenuItem::new("This document's folder").on_click(move |_, _, cx| {
                        if let Some(path) = folder_path.clone() {
                          add_collaborator_scope(&folder_key, crate::app_settings::CollaborationScope::Folder(path));
                          cx.refresh_windows();
                        }
                      }))
                      .item(PopupMenuItem::new("Everything").on_click(move |_, _, cx| {
                        add_collaborator_scope(&global_key, crate::app_settings::CollaborationScope::Global);
                        cx.refresh_windows();
                      }))
                  })
              }),
          )
          .into_any_element()
      }))
      .into_any_element()
  }
}

/// The scope legend — shown wherever grants appear (CO-S3).
fn scope_legend(cx: &Context<CollabShareDialog>) -> AnyElement {
  div()
    .text_xs()
    .text_color(cx.theme().muted_foreground)
    .child("Scopes: Document — just that file · Folder — everything under it · Everything — any document you host.")
    .into_any_element()
}

fn scope_label(scope: &crate::app_settings::CollaborationScope) -> String {
  match scope {
    crate::app_settings::CollaborationScope::Document(path) => format!(
      "Document: {}",
      path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
    ),
    crate::app_settings::CollaborationScope::Folder(path) => format!(
      "Folder: {}",
      path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
    ),
    crate::app_settings::CollaborationScope::Global => "Everything".to_string(),
    crate::app_settings::CollaborationScope::Exclusion(path) => format!("Never: {}", path.display()),
  }
}

fn add_collaborator_scope(identity_key: &str, scope: crate::app_settings::CollaborationScope) {
  let settings = crate::app_settings::load_app_settings();
  let Some(mut person) = settings
    .trusted_collaborators
    .into_iter()
    .find(|person| person.identity_key == identity_key)
  else {
    return;
  };
  person.scopes.push(scope);
  if let Err(error) = crate::app_settings::save_trusted_collaborator(person) {
    tracing::warn!(%error, "adding a trust scope failed");
  }
}

fn remove_collaborator_scope(identity_key: &str, scope_ix: usize) {
  let settings = crate::app_settings::load_app_settings();
  let Some(mut person) = settings
    .trusted_collaborators
    .into_iter()
    .find(|person| person.identity_key == identity_key)
  else {
    return;
  };
  if scope_ix < person.scopes.len() {
    person.scopes.remove(scope_ix);
  }
  if let Err(error) = crate::app_settings::save_trusted_collaborator(person) {
    tracing::warn!(%error, "removing a trust scope failed");
  }
}
