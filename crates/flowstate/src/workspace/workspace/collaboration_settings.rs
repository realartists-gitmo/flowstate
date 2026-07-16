struct CollaborationProfileInputState {
  input: Entity<InputState>,
  persisted_value: String,
  _subscription: Subscription,
}

struct DropboxConnectionInputState {
  app_key: Entity<InputState>,
  root: Entity<InputState>,
}

struct TrustedCollaboratorInputState {
  identity: Entity<InputState>,
  name: Entity<InputState>,
  compared: bool,
  _subscriptions: Vec<Subscription>,
}

struct CollaborationSquadInputState {
  name: Entity<InputState>,
  member: Entity<InputState>,
}

pub(crate) fn render_collaboration_profile(_workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) -> AnyElement {
  {
    let profile = crate::app_settings::load_local_user_profile();
    let state = window.use_keyed_state("collaboration-profile-name", cx, {
      let initial = profile.display_name.clone();
      move |window, cx| {
        let input = cx.new(|cx| {
          InputState::new(window, cx)
            .default_value(initial.clone())
            .placeholder("Your display name")
        });
        let subscription = cx.subscribe_in(
          &input,
          window,
          move |state: &mut CollaborationProfileInputState, input, event: &InputEvent, _, cx| {
            if !matches!(event, InputEvent::Change) {
              return;
            }
            let display_name = input.read(cx).value().trim().to_string();
            if display_name.is_empty() || display_name == state.persisted_value {
              return;
            }
            let current = crate::app_settings::load_local_user_profile();
            if let Err(error) =
              crate::app_settings::save_local_collaboration_profile(display_name.clone(), current.color_rgb, current.avatar_path)
            {
              tracing::warn!(%error, "saving collaboration display name failed");
            } else {
              state.persisted_value = display_name;
            }
          },
        );
        CollaborationProfileInputState {
          input,
          persisted_value: initial,
          _subscription: subscription,
        }
      }
    });

    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_3()
      .child(div().w_40().text_sm().child("Display name"))
      .child(
        div()
          .flex_1()
          .min_w(px(220.0))
          .child(Input::new(&state.read(cx).input).w_full()),
      )
      .into_any_element()
  }
}

pub(crate) fn render_collaboration_discovery_pause(_workspace: WeakEntity<Workspace>, _window: &mut Window, _cx: &mut App) -> AnyElement {
  {
    let settings = crate::app_settings::load_app_settings();
    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_4()
      .child(
        div()
          .flex_1()
          .min_w_0()
          .child(div().text_sm().child("Pause trusted-peer discovery")),
      )
      .child(
        Checkbox::new("collaboration-discovery-paused")
          .small()
          .checked(settings.collaboration_discovery_paused)
          .on_click(move |checked, _, cx| {
            let current = crate::app_settings::load_app_settings();
            if let Err(error) =
              crate::app_settings::save_collaboration_discovery_options(*checked, current.bluetooth_collaboration_discovery_enabled)
            {
              tracing::warn!(%error, "saving discovery pause setting failed");
              return;
            }
            crate::collab::reconfigure_discovery(cx);
          }),
      )
      .into_any_element()
  }
}

pub(crate) fn render_trusted_collaborators(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) -> AnyElement {
  {
    let state = window.use_keyed_state("trusted-collaborator-input", cx, {
      let workspace = workspace.clone();
      move |window, cx| {
        let identity = cx.new(|cx| InputState::new(window, cx).placeholder("Portable identity key"));
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Person's name"));
        let subscriptions = [&identity, &name]
          .into_iter()
          .map(|input| {
            let workspace = workspace.clone();
            cx.subscribe_in(input, window, move |_, _, event: &InputEvent, _, cx| {
              if matches!(event, InputEvent::Change) {
                let _ = workspace.update(cx, |_, cx| cx.notify());
              }
            })
          })
          .collect();
        TrustedCollaboratorInputState {
          identity,
          name,
          compared: false,
          _subscriptions: subscriptions,
        }
      }
    });
    let identity_input = state.read(cx).identity.clone();
    let name_input = state.read(cx).name.clone();
    let identity_text = identity_input.read(cx).value().trim().to_string();
    let remote_key = identity_text.parse::<iroh::PublicKey>().ok();
    let safety_code = remote_key.as_ref().and_then(|remote| {
      crate::app_settings::load_local_identity_secret().map(|local| flowstate_collab::identity::safety_code(&local.public(), remote))
    });
    let active_path = workspace
      .upgrade()
      .and_then(|workspace| workspace.read(cx).active_editor.clone())
      .and_then(|editor| editor.read(cx).document_path().cloned());
    let settings = crate::app_settings::load_app_settings();
    let mut content = v_flex()
      .w_full()
      .gap_2()
      .child(div().text_sm().child("Trusted people"));
    if settings.trusted_collaborators.is_empty() {
      content = content.child(div().text_xs().child("No verified people yet."));
    }
    for (index, collaborator) in settings.trusted_collaborators.into_iter().enumerate() {
      let identity_key = collaborator.identity_key.clone();
      let workspace = workspace.clone();
      content = content.child(
        h_flex()
          .items_center()
          .justify_between()
          .gap_2()
          .child(
            v_flex()
              .min_w_0()
              .child(div().text_sm().child(collaborator.display_name))
              .child(div().text_xs().text_ellipsis().child(identity_key.clone())),
          )
          .child(
            Button::new(("trusted-collaborator-revoke", index))
              .label("Revoke")
              .xsmall()
              .danger()
              .on_click(move |_, _, cx| {
                if let Err(error) = crate::app_settings::remove_trusted_collaborator(&identity_key) {
                  tracing::warn!(%error, "revoking trusted collaborator failed");
                  return;
                }
                crate::collab::reconfigure_discovery(cx);
                let _ = workspace.update(cx, |_, cx| cx.notify());
              }),
          ),
      );
    }
    let state_for_check = state.clone();
    let state_for_add = state.clone();
    let workspace_for_check = workspace.clone();
    let workspace_for_add = workspace.clone();
    content
      .child(div().h(px(1.0)).w_full().bg(cx.theme().border))
      .child(
        h_flex()
          .gap_2()
          .child(Input::new(&name_input).w_full())
          .child(Input::new(&identity_input).w_full()),
      )
      .when_some(safety_code, |this, code| {
        this
          .child(div().text_sm().child(format!("Safety code: {code}")))
          .child(
            h_flex()
              .items_center()
              .justify_between()
              .child(
                div()
                  .text_xs()
                  .child("Compare this code with the other person over a separate channel."),
              )
              .child(
                Checkbox::new("trusted-collaborator-compared")
                  .small()
                  .checked(state.read(cx).compared)
                  .on_click(move |checked, _, cx| {
                    state_for_check.update(cx, |state, cx| {
                      state.compared = *checked;
                      cx.notify();
                    });
                    let _ = workspace_for_check.update(cx, |_, cx| cx.notify());
                  }),
              ),
          )
      })
      .child(
        h_flex()
          .justify_end()
          .items_center()
          .gap_2()
          .when(active_path.is_none(), |this| {
            // CO-S1 (Law 2): the dead end explains itself instead of just
            // graying out.
            this.child(
              div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Open and save a document first — trust is granted per document."),
            )
          })
          .when(active_path.is_some() && remote_key.is_none(), |this| {
            this.child(
              div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Paste their portable identity key to continue."),
            )
          })
          .child(
          Button::new("trusted-collaborator-add")
            .label("Trust for this document")
            .small()
            .primary()
            .disabled(remote_key.is_none() || active_path.is_none() || !state.read(cx).compared)
            .on_click(move |_, window, cx| {
              let identity_key = state_for_add
                .read(cx)
                .identity
                .read(cx)
                .value()
                .trim()
                .to_string();
              let display_name = state_for_add
                .read(cx)
                .name
                .read(cx)
                .value()
                .trim()
                .to_string();
              let Some(path) = active_path.clone() else { return };
              if display_name.is_empty() || identity_key.parse::<iroh::PublicKey>().is_err() {
                return;
              }
              let collaborator = crate::app_settings::TrustedCollaborator {
                identity_key,
                display_name,
                avatar_path: None,
                color_rgb: None,
                verified: true,
                scopes: vec![crate::app_settings::CollaborationScope::Document(path)],
              };
              if let Err(error) = crate::app_settings::save_trusted_collaborator(collaborator) {
                tracing::warn!(%error, "saving trusted collaborator failed");
                return;
              }
              state_for_add.update(cx, |state, cx| {
                state.compared = false;
                state
                  .identity
                  .update(cx, |input, cx| input.set_value("", window, cx));
                state
                  .name
                  .update(cx, |input, cx| input.set_value("", window, cx));
                cx.notify();
              });
              crate::collab::reconfigure_discovery(cx);
              let _ = workspace_for_add.update(cx, |_, cx| cx.notify());
            }),
        ),
      )
      .into_any_element()
  }
}

pub(crate) fn render_collaboration_bluetooth(_workspace: WeakEntity<Workspace>, _window: &mut Window, _cx: &mut App) -> AnyElement {
  {
    let settings = crate::app_settings::load_app_settings();
    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_4()
      .child(
        div()
          .flex_1()
          .min_w_0()
          .child(div().text_sm().child("Nearby Bluetooth discovery"))
          .child(
            div()
              .text_xs()
              .child("Optional. Flowstate only advertises a short-lived signed locator."),
          ),
      )
      .child(
        Checkbox::new("collaboration-bluetooth-enabled")
          .small()
          .checked(settings.bluetooth_collaboration_discovery_enabled)
          .on_click(move |checked, _, cx| {
            let current = crate::app_settings::load_app_settings();
            if let Err(error) = crate::app_settings::save_collaboration_discovery_options(current.collaboration_discovery_paused, *checked) {
              tracing::warn!(%error, "saving Bluetooth discovery setting failed");
              return;
            }
            crate::collab::reconfigure_discovery(cx);
          }),
      )
      .into_any_element()
  }
}

pub(crate) fn render_collaboration_squads(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut App) -> AnyElement {
  {
    let state = window.use_keyed_state("collaboration-squad-input", cx, |window, cx| CollaborationSquadInputState {
      name: cx.new(|cx| InputState::new(window, cx).placeholder("Squad name")),
      member: cx.new(|cx| InputState::new(window, cx).placeholder("Verified member identity key")),
    });
    let name = state.read(cx).name.clone();
    let member = state.read(cx).member.clone();
    let active_folder = workspace
      .upgrade()
      .and_then(|workspace| workspace.read(cx).active_editor.clone())
      .and_then(|editor| {
        editor
          .read(cx)
          .document_path()
          .and_then(|path| path.parent())
          .map(Path::to_path_buf)
      });
    let squads = crate::app_settings::load_app_settings().collaboration_squads;
    let mut content = v_flex()
      .w_full()
      .gap_2()
      .child(div().text_sm().child("Squads"));
    if squads.is_empty() {
      content = content.child(
        div()
          .text_xs()
          .child("No squads yet. Squads share a default folder scope."),
      );
    }
    for (index, squad) in squads.into_iter().enumerate() {
      let id = squad.id.clone();
      let workspace = workspace.clone();
      content = content.child(
        h_flex()
          .items_center()
          .justify_between()
          .child(
            v_flex().child(div().text_sm().child(squad.name)).child(
              div()
                .text_xs()
                .child(format!("{} member(s)", squad.member_identity_keys.len())),
            ),
          )
          .child(
            Button::new(("collaboration-squad-remove", index))
              .label("Remove")
              .xsmall()
              .danger()
              .on_click(move |_, _, cx| {
                if let Err(error) = crate::app_settings::remove_collaboration_squad(&id) {
                  tracing::warn!(%error, "removing collaboration squad failed");
                  return;
                }
                crate::collab::reconfigure_discovery(cx);
                let _ = workspace.update(cx, |_, cx| cx.notify());
              }),
          ),
      );
    }
    let state_for_add = state.clone();
    let workspace_for_add = workspace.clone();
    content
      .child(
        h_flex()
          .gap_2()
          .child(Input::new(&name).w_full())
          .child(Input::new(&member).w_full()),
      )
      .child(
        h_flex()
          .justify_end()
          .items_center()
          .gap_2()
          .when(active_folder.is_none(), |this| {
            this.child(
              div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Open a saved document first — a squad covers that document's folder."),
            )
          })
          .child(
          Button::new("collaboration-squad-add")
            .label("Create folder squad")
            .small()
            .outline()
            .disabled(active_folder.is_none())
            .on_click(move |_, window, cx| {
              let name = state_for_add
                .read(cx)
                .name
                .read(cx)
                .value()
                .trim()
                .to_string();
              let member = state_for_add
                .read(cx)
                .member
                .read(cx)
                .value()
                .trim()
                .to_string();
              let settings = crate::app_settings::load_app_settings();
              let member_is_verified = settings
                .trusted_collaborators
                .iter()
                .any(|contact| contact.verified && contact.identity_key == member);
              let Some(folder) = active_folder.clone() else { return };
              if name.is_empty() || !member_is_verified {
                tracing::warn!("a squad requires a name and a safety-code-verified member");
                return;
              }
              let squad = crate::app_settings::CollaborationSquad {
                id: Uuid::new_v4().to_string(),
                name,
                member_identity_keys: vec![member],
                default_scopes: vec![crate::app_settings::CollaborationScope::Folder(folder)],
              };
              if let Err(error) = crate::app_settings::save_collaboration_squad(squad) {
                tracing::warn!(%error, "saving collaboration squad failed");
                return;
              }
              state_for_add.update(cx, |state, cx| {
                state
                  .name
                  .update(cx, |input, cx| input.set_value("", window, cx));
                state
                  .member
                  .update(cx, |input, cx| input.set_value("", window, cx));
                cx.notify();
              });
              crate::collab::reconfigure_discovery(cx);
              let _ = workspace_for_add.update(cx, |_, cx| cx.notify());
            }),
        ),
      )
      .into_any_element()
  }
}

fn dropbox_connection_item(_workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, window, cx| {
    let settings = crate::app_settings::load_app_settings().dropbox_collaboration;
    let state = window.use_keyed_state("collaboration-dropbox-connection", cx, {
      let app_key = settings.app_key.clone();
      let root = settings.root.clone();
      move |window, cx| DropboxConnectionInputState {
        app_key: cx.new(|cx| {
          InputState::new(window, cx)
            .default_value(app_key)
            .placeholder("Dropbox app key")
        }),
        root: cx.new(|cx| {
          InputState::new(window, cx)
            .default_value(root)
            .placeholder("Optional app-folder path")
        }),
      }
    });
    let app_key = state.read(cx).app_key.clone();
    let root = state.read(cx).root.clone();
    let connected = settings.enabled && !settings.access_token.is_empty();

    v_flex()
      .w_full()
      .gap_2()
      .child(
        h_flex()
          .gap_3()
          .child(div().w_40().text_sm().child("App key"))
          .child(Input::new(&app_key).w_full()),
      )
      .child(
        h_flex()
          .gap_3()
          .child(div().w_40().text_sm().child("Discovery root"))
          .child(Input::new(&root).w_full()),
      )
      .child(
        h_flex()
          .items_center()
          .justify_between()
          .child(
            div()
              .text_xs()
              .child(if connected { "Connected" } else { "Not connected" }),
          )
          .child(if connected {
            Button::new("collaboration-dropbox-disconnect")
              .label("Disconnect")
              .small()
              .outline()
              .on_click(move |_, _, cx| {
                if let Err(error) = crate::app_settings::disconnect_dropbox_collaboration() {
                  tracing::warn!(%error, "disconnecting Dropbox failed");
                  return;
                }
                crate::collab::reconfigure_discovery(cx);
              })
          } else {
            Button::new("collaboration-dropbox-connect")
              .label("Connect Dropbox")
              .small()
              .primary()
              .on_click(move |_, _, cx| {
                let app_key = app_key.read(cx).value().trim().to_string();
                let root = root.read(cx).value().trim().to_string();
                if let Err(error) = crate::app_settings::save_dropbox_connection_draft(app_key, root)
                  .and_then(|_| crate::collab::dropbox_oauth::begin(cx).map_err(std::io::Error::other))
                {
                  tracing::warn!(%error, "starting Dropbox connection failed");
                }
              })
          }),
      )
      .into_any_element()
  })
}

fn dropbox_document_binding_item(workspace: WeakEntity<Workspace>) -> SettingItem {
  SettingItem::render(move |_, _, cx| {
    let path = workspace
      .upgrade()
      .and_then(|workspace| workspace.read(cx).active_editor.clone())
      .and_then(|editor| editor.read(cx).document_path().cloned());
    let binding = path
      .as_deref()
      .and_then(crate::app_settings::load_dropbox_document_binding);
    let connected = crate::app_settings::load_dropbox_collaboration().is_some();
    let label = binding.as_ref().map_or_else(
      || "This document is local only".to_string(),
      |binding| format!("Linked to {}", binding.remote_path),
    );
    let path_for_action = path.clone();
    let is_bound = binding.is_some();
    let workspace_for_action = workspace.clone();
    h_flex()
      .w_full()
      .items_center()
      .justify_between()
      .gap_3()
      .child(
        v_flex()
          .min_w_0()
          .child(div().text_sm().child("Active document"))
          .child(div().text_xs().text_ellipsis().child(label)),
      )
      .child(
        Button::new("collaboration-dropbox-document-binding")
          .label(if is_bound { "Unlink" } else { "Link to Dropbox" })
          .small()
          .outline()
          .disabled(path.is_none() || (!connected && !is_bound))
          .on_click(move |_, _, cx| {
            let Some(path) = path_for_action.clone() else { return };
            let result = if is_bound {
              crate::app_settings::remove_dropbox_document_binding(&path).map(|_| ())
            } else {
              let settings = crate::app_settings::load_app_settings().dropbox_collaboration;
              let Some(filename) = path.file_name().and_then(|name| name.to_str()) else {
                tracing::warn!(path = %path.display(), "Dropbox link requires a Unicode filename");
                return;
              };
              let root = settings.root.trim_matches('/');
              let remote_path = if root.is_empty() {
                format!("/{filename}")
              } else {
                format!("/{root}/{filename}")
              };
              crate::app_settings::save_dropbox_document_binding(crate::app_settings::DropboxDocumentBinding {
                local_path: path,
                remote_path,
                revision: None,
              })
            };
            if let Err(error) = result {
              tracing::warn!(%error, "updating Dropbox document link failed");
            }
            let _ = workspace_for_action.update(cx, |_, cx| cx.notify());
          }),
      )
      .into_any_element()
  })
}
