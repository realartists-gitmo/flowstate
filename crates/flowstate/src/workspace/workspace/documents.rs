#[hotpath::measure_all]
impl Workspace {
  const MAX_PENDING_COLLABORATION_UPDATES: usize = 128;
  const COLLABORATION_CHECKPOINT_DELTA_INTERVAL: usize = 64;

  pub fn new(initial_path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
    let zoom_slider = cx.new(|_| {
      SliderState::new()
        .min(25.0)
        .max(400.0)
        .step(5.0)
        .default_value(100.0)
    });
    let zoom_slider_subscription = cx.subscribe(&zoom_slider, |workspace, _, event: &SliderEvent, cx| {
      let SliderEvent::Change(SliderValue::Single(percent)) = event else {
        return;
      };
      if let Some(editor) = workspace.active_editor.clone() {
        editor.update(cx, |editor, cx| {
          editor.set_zoom_percent(*percent, cx);
        });
      }
    });
    let workspace = cx.entity().downgrade();
    let window_handle = window.window_handle();
    let keybinding_interceptor = cx.intercept_keystrokes(move |event, window, cx| {
      if window.window_handle() != window_handle {
        return;
      }
      let Some(command) = workspace_command_for_keystroke(&event.keystroke) else {
        return;
      };
      if workspace
        .update(cx, |workspace, cx| workspace.handle_window_keybinding(command, window, cx))
        .unwrap_or(false)
      {
        cx.stop_propagation();
      }
    });
    let toolkit_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tub blocks, tags, and analytics"));
    let tub_file_search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search tub"));
    let _tub_file_search_subscription = cx.subscribe(&tub_file_search_input, |workspace, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        workspace.refresh_tub_file_search(cx);
      }
    });
    let _toolkit_search_subscription = cx.subscribe(&toolkit_search_input, |workspace, _, event: &InputEvent, cx| {
      if let InputEvent::Change = event {
        workspace.refresh_toolkit_search(cx);
      }
    });
    let initial_invite = initial_path
      .as_ref()
      .and_then(|path| path.to_str())
      .filter(|value| value.starts_with(FLOWSTATE_INVITE_PREFIX))
      .map(str::to_owned);

    let mut this = Self {
      document_panels: Vec::new(),
      flow_panels: Vec::new(),
      active_document_id: None,
      active_editor: None,
      active_flow: None,
      ribbon_collapsed: false,
      outline_collapsed: false,
      toolkit_collapsed: false,
      active_toolkit_tool: None,
      left_nav_mode: LeftNavMode::Outline,
      tab_bar_scroll_handle: ScrollHandle::new(),
      body_resizable_state: cx.new(|_| ResizableState::default()),
      content_resizable_state: cx.new(|_| ResizableState::default()),
      ribbon_resizable_state: cx.new(|_| ResizableState::default()),
      committed_ribbon_height: px(112.0),
      outline_tree: cx.new(|cx| TreeState::new(cx)),
      outline_cache: None,
      collapsed_outline_items: HashSet::new(),
      outline_revision: 0,
      outline_viewport_paragraph: None,
      outline_scrolled_paragraph: None,
      editor_subscriptions: Vec::new(),
      collaboration_host: None,
      collaboration_last_published_hash: None,
      collaboration_client_updates: None,
      collaboration_pending_updates: VecDeque::new(),
      collaboration_runtime_id: 0,
      collaboration_delta_updates_since_checkpoint: 0,
      collaboration: CollaborationUiState::default(),
      settings_overlay: None,
      document_style_section: DocumentStyleSection::Text,
      settings_section: WorkspaceSettingsSection::General,
      autosave_enabled: load_autosave(),
      autosave_document_generations: HashMap::new(),
      autosave_flow_in_flight: HashSet::new(),
      file_search_overlay: None,
      tub_root: None,
      tub_index: None,
      tub_files: Vec::new(),
      tub_tree: cx.new(|cx| TreeState::new(cx)),
      tub_tree_items: Vec::new(),
      tub_tree_entries: Vec::new(),
      tub_expanded_dirs: HashSet::new(),
      tub_file_search_input,
      tub_file_search_generation: 0,
      tub_status: "No tub selected".into(),
      tub_watcher: None,
      tub_watch_polling: false,
      tub_scan_in_flight: false,
      tub_scan_pending: false,
      active_tub_path: None,
      toolkit_search_input,
      toolkit_search_filter: ToolkitSearchFilter::All,
      toolkit_hits: Vec::new(),
      expanded_toolkit_hits: HashSet::new(),
      toolkit_status: "Select a tub to search evidence.".into(),
      toolkit_search_generation: 0,
      _tub_file_search_subscription,
      _toolkit_search_subscription,
      zoom_slider,
      _zoom_slider_subscription: zoom_slider_subscription,
      _keybinding_interceptor: keybinding_interceptor,
    };

    if let Some(root) = load_tub_root() {
      this.load_tub_root(root, cx);
    }

    if let Some(path) = initial_path.filter(|_| initial_invite.is_none()) {
      // Initial window creation happens before GPUI has produced stable
      // layout bounds for the resizable document area. Documents opened later
      // already run after that first layout pass, so defer startup loading by
      // one frame to give the initial editor the same settled geometry.
      cx.on_next_frame(window, move |workspace, window, cx| {
        workspace.open_document_path(path, window, cx);
      });
    }

    if let Some(invite) = initial_invite {
      cx.on_next_frame(window, move |workspace, window, cx| {
        workspace.join_collaboration_from_invite(invite, window, cx);
      });
    }

    this
  }
  pub fn start_collaboration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(host_snapshot) = self.active_collaboration_host_snapshot(cx) else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "No document open",
        "Open a DB8 or FL0 file before starting collaboration.",
      );
      return;
    };
    self.start_collaboration_from_snapshot(host_snapshot, window, cx);
  }

  fn start_collaboration_from_snapshot(&mut self, host_snapshot: CollaborationHostSnapshot, window: &mut Window, cx: &mut Context<Self>) {
    self.close_collaboration_runtime(cx);
    let panel_id = host_snapshot.panel_id();
    let document_id = host_snapshot.document_id();
    let format_kind = host_snapshot.format_kind();
    let window_handle = window.window_handle();
    let runtime_id = self.collaboration_runtime_id;
    self.collaboration.state = SessionState::Hosting;
    self.collaboration.role = Some("Owner");
    self.collaboration.panel_id = Some(panel_id);
    self.collaboration.document_id = Some(document_id);
    self.collaboration.format_kind = Some(format_kind);
    self.collaboration.pending_invite = None;
    self.collaboration.last_error = None;
    self.collaboration.peers.clear();
    self.collaboration_last_published_hash = None;
    self.collaboration_client_updates = None;
    let start_host = cx.background_executor().spawn(async move {
      let host_input = host_snapshot
        .into_host_input()
        .ok_or_else(|| anyhow::anyhow!("failed to build collaboration source from the active document"))?;
      run_on_sync_runtime(async move {
        HostedCollaboration::start(host_input.document, host_input.assets, Role::Owner)
          .await
          .map(|host| (host, host_input.source_hash))
      })
    });
    cx.spawn(async move |workspace, cx| {
      let result = start_host.await;
      let is_current_runtime = workspace
        .update(cx, |workspace, _| workspace.collaboration_runtime_id == runtime_id)
        .unwrap_or(false);
      if !is_current_runtime {
        if let Ok((host, _)) = result {
          let _ = host.shutdown().await;
        }
        return;
      }
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
          match result {
            Ok((host, source_hash)) => {
              workspace.collaboration_last_published_hash = source_hash.or_else(|| host.document_hash().ok());
              workspace.apply_collaboration_role_to_panel(panel_id, Role::Owner, cx);
              workspace.spawn_host_update_loop(panel_id, host.document_state(), host.subscribe_live_updates(), window_handle, cx);
              workspace.collaboration_host = Some(host);
              workspace.collaboration.state = SessionState::Live;
              workspace.collaboration.role = Some("Owner");
              workspace.collaboration.panel_id = Some(panel_id);
              workspace.collaboration.document_id = Some(document_id);
              workspace.collaboration.format_kind = Some(format_kind);
              workspace.collaboration.last_error = None;
              workspace.collaboration.peers.clear();
              workspace.drain_pending_updates_to_host();
              if let Some(role) = workspace.collaboration.pending_invite_copy_role.take() {
                workspace.issue_collaboration_invite(role, window, cx);
              }
            },
            Err(error) => {
              workspace.collaboration_host = None;
              workspace.collaboration.state = SessionState::Failed;
              workspace.collaboration.last_error = Some(format!("{error:#}"));
              workspace.collaboration.pending_invite_copy_role = None;
              workspace.collaboration.peers.clear();
            },
          }
          cx.notify();
        });
      });
    })
    .detach();
    show_collaboration_prompt(
      window,
      cx,
      PromptLevel::Info,
      "Preparing collaboration",
      "Flowstate is preparing this document and binding a sync endpoint.",
    );
    cx.notify();
  }

  pub fn stop_collaboration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.close_collaboration_runtime(cx);
    self.collaboration.state = SessionState::Closed;
    self.collaboration.role = None;
    self.collaboration.pending_invite = None;
    self.collaboration.last_error = None;
    self.collaboration.peers.clear();
    show_collaboration_prompt(
      window,
      cx,
      PromptLevel::Info,
      "Collaboration disconnected",
      "This workspace is no longer in a collaboration session.",
    );
    cx.notify();
  }

  fn copy_collaboration_invite(&mut self, role: CollaborationInviteRole, window: &mut Window, cx: &mut Context<Self>) {
    if self.collaboration_host.is_some() {
      self.issue_collaboration_invite(role, window, cx);
      return;
    }
    if self.collaboration.role == Some("Owner") && matches!(self.collaboration.state, SessionState::Hosting | SessionState::Reconnecting) {
      self.collaboration.pending_invite_copy_role = Some(role);
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Info,
        "Invite pending",
        "Flowstate is still preparing the owner session. The invite will be copied when collaboration is live.",
      );
      cx.notify();
      return;
    }
    show_collaboration_prompt(
      window,
      cx,
      PromptLevel::Warning,
      "Invite unavailable",
      "Start collaboration as owner before copying invite links.",
    );
  }

  fn issue_collaboration_invite(&mut self, role: CollaborationInviteRole, window: &mut Window, cx: &mut Context<Self>) {
    let Some(host) = self.collaboration_host.as_ref() else {
      self.collaboration.pending_invite_copy_role = Some(role);
      return;
    };
    let role_name = collaboration_role_label(role);
    match host.issue_invite_link(collaboration_sync_role(role), Some(role_name.to_string()), true) {
      Ok(invite) => {
        cx.write_to_clipboard(ClipboardItem::new_string(invite));
        self.collaboration.last_error = None;
        show_collaboration_prompt(
          window,
          cx,
          PromptLevel::Info,
          "Invite copied",
          &format!("{role_name} invite copied to clipboard."),
        );
      },
      Err(error) => {
        let detail = format!("{error:#}");
        self.collaboration.last_error = Some(detail.clone());
        show_collaboration_prompt(window, cx, PromptLevel::Critical, "Invite unavailable", &detail);
      },
    }
    cx.notify();
  }

  pub fn revoke_collaboration_invites(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(host) = self.collaboration_host.as_ref() else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "Owner session required",
        "Start collaboration as owner before revoking invite links.",
      );
      return;
    };
    match host.revoke_all_invites() {
      Ok(count) => {
        self.collaboration.last_error = None;
        show_collaboration_prompt(
          window,
          cx,
          PromptLevel::Info,
          "Invites revoked",
          &format!("Revoked {count} active collaboration invite(s). Connected peers remain in the session."),
        );
      },
      Err(error) => {
        let detail = format!("{error:#}");
        self.collaboration.last_error = Some(detail.clone());
        show_collaboration_prompt(window, cx, PromptLevel::Critical, "Invite revoke failed", &detail);
      },
    }
    cx.notify();
  }

  pub fn join_collaboration_from_clipboard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(item) = cx.read_from_clipboard() else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "No invite on clipboard",
        "Copy a flowstate://collab/ invite link before joining.",
      );
      return;
    };
    let Some(invite) = item.text() else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "No invite on clipboard",
        "The current clipboard item does not contain text.",
      );
      return;
    };
    self.join_collaboration_from_invite(invite.trim().to_string(), window, cx);
  }

  pub fn join_collaboration_from_invite(&mut self, invite: String, window: &mut Window, cx: &mut Context<Self>) {
    self.spawn_join_collaboration_runtime(invite, None, window, cx);
  }

  fn spawn_join_collaboration_runtime(&mut self, invite: String, target_panel_id: Option<Uuid>, window: &mut Window, cx: &mut Context<Self>) {
    self.collaboration.state = SessionState::Joining;
    self.collaboration.pending_invite = Some(invite.clone());
    let ticket = match decode_invite_link(&invite) {
      Ok(ticket) => ticket,
      Err(error) => {
        let detail = format!("{error:#}");
        self.collaboration.state = SessionState::Failed;
        self.collaboration.role = None;
        self.collaboration.last_error = Some(detail.clone());
        show_collaboration_prompt(window, cx, PromptLevel::Critical, "Join failed", &detail);
        cx.notify();
        return;
      },
    };
    if target_panel_id.is_some() {
      self.close_collaboration_runtime_preserving_pending(cx);
    } else {
      self.close_collaboration_runtime(cx);
    }
    self.collaboration.state = if target_panel_id.is_some() {
      SessionState::Reconnecting
    } else {
      SessionState::Joining
    };
    self.collaboration.pending_invite = Some(invite.clone());
    self.collaboration.role = Some(collaboration_sync_role_label(ticket.invited_role));
    self.collaboration.panel_id = target_panel_id;
    self.collaboration.document_id = Some(ticket.document_id);
    self.collaboration.format_kind = Some(ticket.format_kind);
    self.collaboration.peers.clear();
    let runtime_id = self.collaboration_runtime_id;
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let mut target_panel_id = target_panel_id;
      let mut reconnect_delay = std::time::Duration::from_millis(500);
      loop {
        let should_continue = window_handle
          .update(cx, |_, _, cx| {
            workspace
              .update(cx, |workspace, _| {
                workspace.collaboration_runtime_id == runtime_id
                  && workspace.collaboration.pending_invite.as_deref() == Some(invite.as_str())
                  && target_panel_id
                    .is_none_or(|panel_id| workspace.collaboration.panel_id.is_none() || workspace.collaboration.panel_id == Some(panel_id))
              })
              .unwrap_or(false)
          })
          .unwrap_or(false);
        if !should_continue {
          break;
        }

        let client: anyhow::Result<_> = cx
          .background_executor()
          .spawn({
            let invite = invite.clone();
            async move {
              run_on_sync_runtime(async move {
                let mut client = connect_live_invite(&invite).await?;
                client.request_document_assets().await?;
                Ok(client)
              })
            }
          })
          .await;
        match client {
          Ok(mut client) => {
            let (update_tx, mut update_rx) = mpsc::unbounded_channel::<PendingCollaborationUpdate>();
            let authorized_role = client.authorization.role;
            let joined_document_id = client.document.document_id();
            let joined_format_kind = client.document.format_kind();
            let joined_hash = client.document.projection_hash().ok();
            let initial = collab_document_to_workspace_document(client.document.clone());
            let collaboration_panel_id = window_handle
              .update(cx, |_, window, cx| {
                workspace
                  .update(cx, |workspace, cx| {
                    let had_pending_updates = !workspace.collaboration_pending_updates.is_empty();
                    let panel_id = if let Some(panel_id) = target_panel_id {
                      if workspace.collaboration.panel_id != Some(panel_id) {
                        return None;
                      }
                      if !had_pending_updates
                        && let Err(error) = workspace.apply_collaboration_source_to_panel(panel_id, client.document.clone(), None, cx)
                      {
                        workspace.collaboration.state = SessionState::Failed;
                        workspace.collaboration.last_error = Some(format!("{error:#}"));
                        cx.notify();
                        return None;
                      }
                      panel_id
                    } else {
                      match initial {
                        Ok(JoinedWorkspaceDocument::Document(document)) => {
                          let panel =
                            workspace.add_document_panel_with_title(*document, None, Some("Collaboration.db8".to_string()), window, cx);
                          panel.read(cx).id()
                        },
                        Ok(JoinedWorkspaceDocument::Flow(document)) => {
                          let panel = workspace.add_flow_panel(document, None, window, cx);
                          panel.read(cx).id()
                        },
                        Err(error) => {
                          workspace.collaboration.state = SessionState::Failed;
                          workspace.collaboration.last_error = Some(format!("{error:#}"));
                          cx.notify();
                          return None;
                        },
                      }
                    };
                    workspace.apply_collaboration_role_to_panel(panel_id, authorized_role, cx);
                    workspace.collaboration_client_updates = Some(update_tx.clone());
                    workspace.collaboration.state = SessionState::Live;
                    workspace.collaboration.panel_id = Some(panel_id);
                    workspace.collaboration.document_id = Some(joined_document_id);
                    workspace.collaboration.format_kind = Some(joined_format_kind);
                    workspace.collaboration_last_published_hash = joined_hash;
                    workspace.collaboration.last_error = None;
                    workspace.collaboration.peers.clear();
                    workspace.drain_pending_updates_to_sender(&update_tx);
                    cx.notify();
                    Some(panel_id)
                  })
                  .ok()
                  .flatten()
              })
              .ok()
              .flatten();
            let Some(collaboration_panel_id) = collaboration_panel_id else {
              let _ = cx
                .background_executor()
                .spawn(async move { run_on_sync_runtime(client.shutdown()) })
                .await;
              return;
            };
            target_panel_id = Some(collaboration_panel_id);
            reconnect_delay = std::time::Duration::from_millis(500);
            let mut local_disconnect = false;
            loop {
              let (next_client, next_update_rx, received, handled_local_update, disconnected, update_error) = cx
                .background_executor()
                .spawn(async move {
                  let mut update_error = None;
                  let mut handled_local_update = false;
                  let mut disconnected = false;
                  let received = run_on_sync_runtime(async {
                    tokio::select! {
                      update = client.receive_next_update() => update,
                      update = update_rx.recv() => {
                        match update {
                          Some(update) => {
                            let result = match update {
                              PendingCollaborationUpdate::Source { source, application, .. } => {
                                client.replace_source_from(&source, application).await
                              },
                              PendingCollaborationUpdate::Application { application } => client.publish_application_update(application).await,
                              PendingCollaborationUpdate::Presence { cursor } => {
                                client.publish_presence("", cursor, None, None).await
                              },
                            };
                            if let Err(error) = result {
                              update_error = Some(format!("{error:#}"));
                            }
                            handled_local_update = true;
                            Ok(None)
                          },
                          None => {
                            disconnected = true;
                            Ok(None)
                          },
                        }
                      },
                    }
                  });
                  (client, update_rx, received, handled_local_update, disconnected, update_error)
                })
                .await;
              client = next_client;
              update_rx = next_update_rx;
              if let Some(detail) = update_error {
                let _ = window_handle.update(cx, |_, _, cx| {
                  let _ = workspace.update(cx, |workspace, cx| {
                    if workspace.collaboration.panel_id == Some(collaboration_panel_id) {
                      workspace.collaboration.last_error = Some(detail);
                      cx.notify();
                    }
                  });
                });
              }
              if disconnected {
                local_disconnect = true;
              }
              if handled_local_update {
                continue;
              }
              let Ok(Some(event)) = received else {
                break;
              };
              match event {
                SessionEvent::SnapshotApplied { .. } | SessionEvent::UpdateApplied { .. } => {
                  let application = client.last_application.clone();
                  let source = client.document.clone();
                  let _ = window_handle.update(cx, |_, _, cx| {
                    let _ = workspace.update(cx, |workspace, cx| {
                      if let Err(error) = workspace.apply_collaboration_source_to_panel(collaboration_panel_id, source, application, cx) {
                        workspace.collaboration.last_error = Some(format!("{error:#}"));
                      } else {
                        workspace.collaboration.last_error = None;
                      }
                      cx.notify();
                    });
                  });
                },
                event => {
                  let _ = window_handle.update(cx, |_, _, cx| {
                    let _ = workspace.update(cx, |workspace, cx| {
                      workspace.apply_collaboration_session_event(collaboration_panel_id, event, cx);
                    });
                  });
                },
              }
            }
            let _ = cx
              .background_executor()
              .spawn(async move { run_on_sync_runtime(client.shutdown()) })
              .await;
            if !local_disconnect {
              cx.background_executor().timer(reconnect_delay).await;
              reconnect_delay = (reconnect_delay * 2).min(std::time::Duration::from_secs(5));
              continue;
            }
            break;
          },
          Err(error) => {
            let _ = window_handle.update(cx, |_, _, cx| {
              let _ = workspace.update(cx, |workspace, cx| {
                workspace.collaboration.state = if target_panel_id.is_some() {
                  SessionState::Reconnecting
                } else {
                  SessionState::Failed
                };
                workspace.collaboration.last_error = Some(format!("{error:#}"));
                workspace.collaboration_client_updates = None;
                cx.notify();
              });
            });
            if target_panel_id.is_none() {
              break;
            }
            cx.background_executor().timer(reconnect_delay).await;
            reconnect_delay = (reconnect_delay * 2).min(std::time::Duration::from_secs(5));
          },
        }
      }
    })
    .detach();
    show_collaboration_prompt(
      window,
      cx,
      PromptLevel::Info,
      if target_panel_id.is_some() {
        "Reconnecting collaboration"
      } else {
        "Joining collaboration"
      },
      "Flowstate is connecting to the host and requesting the source snapshot.",
    );
    cx.notify();
  }

  pub fn reconnect_collaboration(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(invite) = self.collaboration.pending_invite.clone() {
      let target_panel_id = self.collaboration.panel_id;
      self.collaboration.state = SessionState::Reconnecting;
      self.collaboration.last_error = None;
      self.spawn_join_collaboration_runtime(invite, target_panel_id, window, cx);
      return;
    }
    if self.collaboration.role == Some("Owner")
      && let Some(panel_id) = self.collaboration.panel_id
      && let Some(host_snapshot) = self.collaboration_host_snapshot_for_panel(panel_id, cx)
    {
      self.close_collaboration_runtime(cx);
      self.set_active_collaboration_panel(panel_id, cx);
      self.collaboration.state = SessionState::Reconnecting;
      self.collaboration.last_error = None;
      self.start_collaboration_from_snapshot(host_snapshot, window, cx);
      return;
    }
    if self.collaboration.role.is_none() {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "Reconnect unavailable",
        "There is no previous collaboration session or invite to reconnect.",
      );
      return;
    }
    self.collaboration.last_error = Some("No reconnect path is available for this collaboration state.".to_string());
    show_collaboration_prompt(
      window,
      cx,
      PromptLevel::Warning,
      "Reconnect unavailable",
      "No reconnect path is available for this collaboration state.",
    );
    cx.notify();
  }

  pub fn promote_collaboration_peers_to_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.set_collaboration_peers_role(Role::Editor, window, cx);
  }

  pub fn downgrade_collaboration_peers_to_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.set_collaboration_peers_role(Role::Viewer, window, cx);
  }

  pub fn set_collaboration_peer_role(&mut self, session_id: SessionId, role: Role, window: &mut Window, cx: &mut Context<Self>) {
    let Some(host) = self.collaboration_host.as_ref() else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "Owner session required",
        "Start or reconnect as owner before changing peer roles.",
      );
      return;
    };
    let peer_label = self
      .collaboration
      .peers
      .get(&session_id)
      .map(collaboration_peer_display_name)
      .unwrap_or_else(|| format!("Peer {}", collaboration_short_uuid(session_id.0)));
    match host.set_peer_role(session_id, role) {
      Ok(true) => {
        if let Some(peer) = self.collaboration.peers.get_mut(&session_id) {
          peer.role = role;
        }
        self.collaboration.last_error = None;
        let role_label = collaboration_sync_role_label(role);
        show_collaboration_prompt(
          window,
          cx,
          PromptLevel::Info,
          "Peer role updated",
          &format!("{peer_label} is now a {role_label}."),
        );
      },
      Ok(false) => {
        self.collaboration.last_error = Some(format!("{peer_label} is not connected."));
        show_collaboration_prompt(
          window,
          cx,
          PromptLevel::Warning,
          "Peer not connected",
          &format!("{peer_label} is not connected."),
        );
      },
      Err(error) => {
        let detail = format!("{error:#}");
        self.collaboration.last_error = Some(detail.clone());
        show_collaboration_prompt(window, cx, PromptLevel::Warning, "Peer role update failed", &detail);
      },
    }
    cx.notify();
  }

  fn set_collaboration_peers_role(&mut self, role: Role, window: &mut Window, cx: &mut Context<Self>) {
    let Some(host) = self.collaboration_host.as_ref() else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "Owner session required",
        "Start or reconnect as owner before changing peer roles.",
      );
      return;
    };
    let peer_ids = self.collaboration.peers.keys().copied().collect::<Vec<_>>();
    if peer_ids.is_empty() {
      show_collaboration_prompt(window, cx, PromptLevel::Info, "No peers connected", "There are no live peers to update.");
      return;
    }
    let mut changed = 0usize;
    let mut last_error = None;
    for session_id in peer_ids {
      match host.set_peer_role(session_id, role) {
        Ok(true) => changed += 1,
        Ok(false) => {},
        Err(error) => last_error = Some(format!("{error:#}")),
      }
    }
    self.collaboration.last_error = last_error;
    let role_label = collaboration_sync_role_label(role);
    show_collaboration_prompt(
      window,
      cx,
      PromptLevel::Info,
      "Peer roles updated",
      &format!("Updated {changed} live peer(s) to {role_label}."),
    );
    cx.notify();
  }

  pub fn kick_collaboration_peer(&mut self, session_id: SessionId, window: &mut Window, cx: &mut Context<Self>) {
    let Some(host) = self.collaboration_host.as_ref() else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "Owner session required",
        "Start or reconnect as owner before removing peers.",
      );
      return;
    };
    let peer_label = self
      .collaboration
      .peers
      .get(&session_id)
      .map(collaboration_peer_display_name)
      .unwrap_or_else(|| format!("Peer {}", collaboration_short_uuid(session_id.0)));
    match host.kick_peer(session_id) {
      Ok(true) => {
        self.collaboration.peers.remove(&session_id);
        self.collaboration.last_error = None;
        show_collaboration_prompt(
          window,
          cx,
          PromptLevel::Info,
          "Peer removed",
          &format!("Removed {peer_label} from this collaboration session."),
        );
      },
      Ok(false) => {
        self.collaboration.last_error = Some(format!("{peer_label} is not connected."));
        show_collaboration_prompt(
          window,
          cx,
          PromptLevel::Warning,
          "Peer not connected",
          &format!("{peer_label} is not connected."),
        );
      },
      Err(error) => {
        let detail = format!("{error:#}");
        self.collaboration.last_error = Some(detail.clone());
        show_collaboration_prompt(window, cx, PromptLevel::Warning, "Peer removal failed", &detail);
      },
    }
    cx.notify();
  }

  pub fn kick_collaboration_peers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(host) = self.collaboration_host.as_ref() else {
      show_collaboration_prompt(
        window,
        cx,
        PromptLevel::Warning,
        "Owner session required",
        "Start or reconnect as owner before removing peers.",
      );
      return;
    };
    let peer_ids = self.collaboration.peers.keys().copied().collect::<Vec<_>>();
    if peer_ids.is_empty() {
      show_collaboration_prompt(window, cx, PromptLevel::Info, "No peers connected", "There are no live peers to remove.");
      return;
    }
    let mut kicked = 0usize;
    let mut last_error = None;
    for session_id in peer_ids {
      match host.kick_peer(session_id) {
        Ok(true) => kicked += 1,
        Ok(false) => {},
        Err(error) => last_error = Some(format!("{error:#}")),
      }
    }
    self.collaboration.last_error = last_error;
    show_collaboration_prompt(
      window,
      cx,
      PromptLevel::Info,
      "Peers removed",
      &format!("Removed {kicked} live peer(s) from this collaboration session."),
    );
    cx.notify();
  }

  pub fn show_collaboration_diagnostics(&self, window: &mut Window, cx: &mut Context<Self>) {
    let role = self.collaboration.role.unwrap_or("None");
    let pending_invite = if self.collaboration.pending_invite.is_some() { "yes" } else { "no" };
    let owner_host = if self.collaboration_host.is_some() { "yes" } else { "no" };
    let pending_invite_copy = self
      .collaboration
      .pending_invite_copy_role
      .map(collaboration_role_label)
      .unwrap_or("none");
    let panel = self
      .collaboration
      .panel_id
      .map(|id| id.to_string())
      .unwrap_or_else(|| "none".to_string());
    let document = self
      .collaboration
      .document_id
      .map(|id| id.0.to_string())
      .unwrap_or_else(|| "none".to_string());
    let peer_count = self.collaboration.peers.len();
    let pending_updates = self.collaboration_pending_updates.len();
    let peer_details = if self.collaboration.peers.is_empty() {
      "none".to_string()
    } else {
      self
        .collaboration
        .peers
        .iter()
        .take(8)
        .map(|(session_id, peer)| {
          let cursor = peer.cursor.as_deref().unwrap_or("none");
          let focus = peer.focus.as_deref().unwrap_or("none");
          format!(
            "{} [{}] session={} cursor={cursor} focus={focus}",
            collaboration_peer_display_name(peer),
            collaboration_sync_role_label(peer.role),
            collaboration_short_uuid(session_id.0),
          )
        })
        .collect::<Vec<_>>()
        .join("\n")
    };
    let detail = format!(
      "State: {:?}\nRole: {role}\nOwner host: {owner_host}\nPeers: {peer_count}\nQueued local updates: {pending_updates}\nPeer details:\n{peer_details}\nPanel: {panel}\nDocument: {document}\nFormat: {:?}\nPending join invite: {pending_invite}\nPending invite copy: {pending_invite_copy}\nLast error: {}",
      self.collaboration.state,
      self.collaboration.format_kind,
      self.collaboration.last_error.as_deref().unwrap_or("None"),
    );
    show_collaboration_prompt(window, cx, PromptLevel::Info, "Collaboration diagnostics", &detail);
  }

  fn create_document_panel(
    &mut self,
    mut document: Document,
    path: Option<PathBuf>,
    title: Option<String>,
    _window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<DocumentPanel> {
    // DB8 stores style assignments, not style appearance. The render theme is
    // local user preference loaded from app settings.
    document.theme = load_document_theme();
    let editor = cx.new(|cx| RichTextEditor::new_with_path(document, path.clone(), cx));
    let smart_word_selection = load_smart_word_selection();
    editor.update(cx, |editor, cx| {
      editor.set_smart_word_selection(smart_word_selection, cx);
    });
    let workspace = cx.entity().downgrade();
    let title = title
      .or_else(|| {
        path
          .as_ref()
          .and_then(|path| path.file_name())
          .map(|name| name.to_string_lossy().to_string())
      })
      .or_else(|| Some(self.next_untitled_title(cx)));
    if let Some(title) = title.clone() {
      editor.update(cx, |editor, cx| {
        editor.set_document_display_name(title.into(), cx);
      });
    }
    let panel = cx.new(|cx| DocumentPanel::new_with_title(title, path, editor.clone(), workspace, cx));
    let id = panel.read(cx).id();
    self.editor_subscriptions.push((
      id,
      cx.observe(&editor, move |workspace, editor, cx| {
        let viewport_paragraph = workspace.active_editor_viewport_paragraph(cx);
        if workspace.outline_viewport_paragraph != viewport_paragraph {
          workspace.outline_viewport_paragraph = viewport_paragraph;
          cx.notify();
        }
        workspace.publish_db8_presence(id, editor.clone(), cx);
        workspace.maybe_autosave_document(id, editor.clone(), cx);
        workspace.publish_db8_collaboration_edit(id, editor.clone(), cx);
      }),
    ));
    self.active_document_id = Some(id);
    self.active_editor = Some(editor);
    self.active_flow = None;
    self.document_panels.push(panel.clone());
    panel
  }

  pub fn set_active_document(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    self.active_document_id = Some(panel_id);
    self.active_editor = Some(editor);
    self.active_flow = None;
    self.refresh_outline_tree(cx);
    cx.notify();
  }

  pub fn set_active_flow(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    self.active_document_id = Some(panel_id);
    self.active_editor = None;
    self.active_flow = Some(editor);
    self.outline_cache = None;
    self.outline_viewport_paragraph = None;
    self.outline_scrolled_paragraph = None;
    cx.notify();
  }

  pub fn remove_document_panel(&mut self, panel_id: Uuid, _: &mut Window, cx: &mut Context<Self>) {
    if self.collaboration.panel_id == Some(panel_id) {
      self.close_collaboration_runtime(cx);
      self.collaboration.state = SessionState::Closed;
      self.collaboration.role = None;
      self.collaboration.pending_invite = None;
      self.collaboration.last_error = None;
      self.collaboration.peers.clear();
    }
    let closing_active_document = self.active_document_id == Some(panel_id);
    if let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      let editor = panel.read(cx).editor();
      editor.update(cx, |editor, _| editor.dispose_for_close());
    }
    if let Some(panel) = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      let editor = panel.read(cx).editor();
      editor.update(cx, |editor, _| editor.discard_recovery_file());
    }
    self
      .document_panels
      .retain(|panel| panel.read(cx).id() != panel_id);
    self
      .flow_panels
      .retain(|panel| panel.read(cx).id() != panel_id);
    self.editor_subscriptions.retain(|(id, _)| *id != panel_id);
    if closing_active_document {
      if let Some(panel) = self.document_panels.last() {
        self.active_document_id = Some(panel.read(cx).id());
        self.active_editor = Some(panel.read(cx).editor());
        self.active_flow = None;
      } else if let Some(panel) = self.flow_panels.last() {
        self.active_document_id = Some(panel.read(cx).id());
        self.active_editor = None;
        self.active_flow = Some(panel.read(cx).editor());
      } else {
        self.active_document_id = None;
        self.active_editor = None;
        self.active_flow = None;
      }
      self.outline_cache = None;
      self.outline_viewport_paragraph = self
        .active_editor
        .as_ref()
        .and_then(|editor| editor.read(cx).viewport_anchor_paragraph());
      self.outline_scrolled_paragraph = None;
    }
    if self.active_document_id.is_none() {
      self.outline_cache = None;
      self.outline_viewport_paragraph = None;
      self.outline_scrolled_paragraph = None;
      self.collapsed_outline_items.clear();
      self
        .outline_tree
        .update(cx, |tree, cx| tree.set_items(Vec::<TreeItem>::new(), cx));
    } else if closing_active_document {
      self.refresh_outline_tree(cx);
    }
    cx.notify();
  }

  pub fn new_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.add_document_panel(new_blank_document(), None, window, cx);
  }

  pub fn new_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    self.add_flow_panel(flowstate_flow::FlowDocument::new(), None, window, cx);
  }

  pub fn open_demo_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let path = PathBuf::from("data/demo.db8");
    self.open_document_path(path, window, cx);
  }

  pub fn open_document_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
    self.open_document_path_with_target(path, None, window, cx);
  }

  pub fn open_document_path_at_paragraph(&mut self, path: PathBuf, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    self.open_document_path_with_target(path, Some(paragraph_ix), window, cx);
  }

  fn open_document_path_with_target(&mut self, path: PathBuf, target_paragraph_ix: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let path_for_error = path.clone();
      let loaded = cx
        .background_executor()
        .spawn(async move { load_workspace_document(path) })
        .await;
      match loaded {
        Ok(LoadedWorkspaceDocument::Document { document, path, title }) => {
          let _ = window_handle.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace.add_document_panel_with_title(*document, path, title, window, cx);
              if let Some(paragraph_ix) = target_paragraph_ix {
                workspace.scroll_active_editor_to_paragraph(paragraph_ix, window, cx);
                cx.on_next_frame(window, move |workspace, window, cx| {
                  workspace.scroll_active_editor_to_paragraph(paragraph_ix, window, cx);
                });
              }
            });
          });
        },
        Ok(LoadedWorkspaceDocument::Flow { document, path }) => {
          let _ = window_handle.update(cx, |_, window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
              workspace.add_flow_panel(document, Some(path), window, cx);
            });
          });
        },
        Err(error) => {
          let detail = format!("Failed to open {}: {error}", path_for_error.display());
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Open failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
        },
      }
    })
    .detach();
  }

  pub fn prompt_open_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
      files: true,
      directories: false,
      multiple: false,
      prompt: Some("Open .db8, .docx, .pdf, or .fl0 document".into()),
    });
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let Ok(Ok(Some(paths))) = paths.await else {
        return;
      };
      let Some(path) = paths.into_iter().next() else {
        return;
      };
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = workspace.update(cx, |workspace, cx| {
          workspace.open_document_path(path, window, cx);
        });
      });
    })
    .detach();
  }

  fn add_document_panel(&mut self, document: Document, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
    self.create_document_panel(document, path, None, window, cx);
    cx.notify();
  }

  fn add_document_panel_with_title(
    &mut self,
    document: Document,
    path: Option<PathBuf>,
    title: Option<String>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<DocumentPanel> {
    let panel = self.create_document_panel(document, path, title, window, cx);
    cx.notify();
    panel
  }

  fn create_flow_panel(
    &mut self,
    document: flowstate_flow::FlowDocument,
    path: Option<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<FlowPanel> {
    let editor = cx.new(|cx| FlowEditor::new_with_path(document, path.clone(), window, cx));
    let workspace = cx.entity().downgrade();
    let title = path
      .as_ref()
      .and_then(|path| path.file_name())
      .map(|name| name.to_string_lossy().to_string())
      .or_else(|| Some(self.next_untitled_flow_title(cx)));
    let panel = cx.new(|cx| FlowPanel::new_with_title(title, path, editor.clone(), workspace, window, cx));
    let id = panel.read(cx).id();
    self.editor_subscriptions.push((
      id,
      cx.observe(&editor, move |workspace, editor, cx| {
        workspace.maybe_autosave_flow(id, editor.clone(), cx);
        workspace.publish_fl0_collaboration_edit(id, editor.clone(), cx);
      }),
    ));
    self.active_document_id = Some(id);
    self.active_editor = None;
    self.active_flow = Some(editor);
    self.flow_panels.push(panel.clone());
    self.outline_cache = None;
    self.outline_viewport_paragraph = None;
    self.outline_scrolled_paragraph = None;
    panel
  }

  fn add_flow_panel(
    &mut self,
    document: flowstate_flow::FlowDocument,
    path: Option<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Entity<FlowPanel> {
    let panel = self.create_flow_panel(document, path, window, cx);
    cx.notify();
    panel
  }

  pub fn close_document_panel(&mut self, panel_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
    let document_panel = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .cloned();
    let flow_panel = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .cloned();
    let Some(panel_kind) = document_panel
      .map(|panel| {
        let editor = panel.read(cx).editor();
        PanelKind::Document { panel, editor }
      })
      .or_else(|| {
        flow_panel.map(|panel| {
          let editor = panel.read(cx).editor();
          PanelKind::Flow { panel, editor }
        })
      })
    else {
      return;
    };
    if !panel_kind.is_dirty(cx) {
      self.remove_document_panel(panel_id, window, cx);
      return;
    }

    let answer = window.prompt(
      PromptLevel::Warning,
      "Save changes before closing?",
      Some("This document has unsaved changes."),
      &[PromptButton::ok("Save"), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
      cx,
    );
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let should_close = match answer.await {
        Ok(0) => match panel_kind.save(window_handle, cx).await {
          PanelSaveOutcome::Saved => true,
          PanelSaveOutcome::Cancelled => false,
          PanelSaveOutcome::Failed(error) => {
            show_save_failed(window_handle, cx, error);
            false
          },
        },
        Ok(1) => {
          panel_kind.discard(cx);
          true
        },
        _ => false,
      };

      if should_close {
        let _ = window_handle.update(cx, |_, window, cx| {
          let _ = workspace.update(cx, |workspace, cx| workspace.remove_document_panel(panel_id, window, cx));
        });
      }
    })
    .detach();
  }

  fn request_close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let dirty_panels = self.dirty_panels(cx);
    if dirty_panels.is_empty() {
      window.remove_window();
      return;
    }

    let message = if dirty_panels.len() == 1 {
      "This document has unsaved changes."
    } else {
      "One or more documents have unsaved changes."
    };
    let answer = window.prompt(
      PromptLevel::Warning,
      "Save changes before closing?",
      Some(message),
      &[PromptButton::ok("Save"), PromptButton::new("Don't Save"), PromptButton::cancel("Cancel")],
      cx,
    );
    let window_handle = window.window_handle();

    cx.spawn(async move |_, cx| {
      let should_close = match answer.await {
        Ok(0) => {
          let mut ok = true;
          for panel in dirty_panels {
            match panel.save(window_handle, cx).await {
              PanelSaveOutcome::Saved => {},
              PanelSaveOutcome::Cancelled => {
                ok = false;
                break;
              },
              PanelSaveOutcome::Failed(error) => {
                ok = false;
                show_save_failed(window_handle, cx, error);
                break;
              },
            }
          }
          ok
        },
        Ok(1) => {
          for panel in dirty_panels {
            panel.discard(cx);
          }
          true
        },
        _ => false,
      };

      if should_close {
        let _ = window_handle.update(cx, |_, window, _| window.remove_window());
      }
    })
    .detach();
  }

  pub fn save_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = self.active_editor.clone() {
      if editor.read(cx).document_path().is_none() {
        self.prompt_save_active_as(editor, window, cx);
        return;
      }
      let save_task = editor.update(cx, |editor, cx| editor.save(cx));
      let window_handle = window.window_handle();
      cx.spawn(async move |_, cx| {
        if let Err(error) = save_task.await {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
        }
      })
      .detach();
      cx.notify();
      return;
    }
    if let Some(editor) = self.active_flow.clone() {
      if editor.read(cx).document_path().is_none() {
        self.prompt_save_active_flow_as(editor, window, cx);
        return;
      }
      let save_task = editor.update(cx, |editor, cx| editor.save(cx));
      let window_handle = window.window_handle();
      cx.spawn(async move |_, cx| {
        if let Err(error) = save_task.await {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
        }
      })
      .detach();
      cx.notify();
    }
  }

  pub fn save_active_as(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(editor) = self.active_editor.clone() {
      self.prompt_save_active_as(editor, window, cx);
    } else if let Some(editor) = self.active_flow.clone() {
      self.prompt_save_active_flow_as(editor, window, cx);
    }
  }

  pub fn close_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else {
      return;
    };
    self.close_document_panel(panel_id, window, cx);
  }

  pub fn open_file_search_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if let Some(overlay) = self.file_search_overlay.clone() {
      overlay.update(cx, |overlay, cx| overlay.focus_search(window, cx));
      return;
    }

    let workspace = cx.entity().downgrade();
    let tub_search = self.active_tub_index_for_search();
    let overlay = cx.new(|cx| FileSearchOverlay::new(workspace, tub_search, window, cx));
    overlay.update(cx, |overlay, cx| overlay.focus_search(window, cx));
    self.file_search_overlay = Some(overlay);
    cx.notify();
  }

  pub fn close_file_search_overlay(&mut self, cx: &mut Context<Self>) {
    self.file_search_overlay = None;
    cx.notify();
  }

  fn maybe_autosave_document(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    if !self.autosave_enabled {
      return;
    }

    let (has_path, has_unsaved_changes, generation) = {
      let editor = editor.read(cx);
      (editor.document_path().is_some(), editor.has_unsaved_changes(), editor.edit_generation())
    };
    if !has_path || !has_unsaved_changes {
      return;
    }
    if self.autosave_document_generations.get(&panel_id) == Some(&generation) {
      return;
    }
    self
      .autosave_document_generations
      .insert(panel_id, generation);
    let save_task = editor.update(cx, |editor, cx| editor.save(cx));
    cx.spawn(async move |workspace, cx| {
      if let Err(error) = save_task.await {
        eprintln!("autosave failed: {error}");
        let _ = workspace.update(cx, |workspace, _| {
          if workspace.autosave_document_generations.get(&panel_id) == Some(&generation) {
            workspace.autosave_document_generations.remove(&panel_id);
          }
        });
      }
    })
    .detach();
  }

  fn maybe_autosave_flow(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    if !self.autosave_enabled
      || self.autosave_flow_in_flight.contains(&panel_id)
      || editor.read(cx).document_path().is_none()
      || !editor.read(cx).has_unsaved_changes()
    {
      return;
    }

    self.autosave_flow_in_flight.insert(panel_id);
    let save_task = editor.update(cx, |editor, cx| editor.save(cx));
    cx.spawn(async move |workspace, cx| {
      if let Err(error) = save_task.await {
        eprintln!("flow autosave failed: {error}");
      }
      let _ = workspace.update(cx, |workspace, _| {
        workspace.autosave_flow_in_flight.remove(&panel_id);
      });
    })
    .detach();
  }

  fn prompt_save_active_as(&mut self, editor: Entity<RichTextEditor>, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else {
      return;
    };
    let save_path = cx.prompt_for_new_path(&default_save_directory(), Some(UNTITLED_DOCUMENT_NAME));
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let path = match save_path.await {
        Ok(Ok(Some(path))) => normalize_db8_path(path),
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
          return;
        },
        Err(error) => {
          eprintln!("save dialog was canceled before completion: {error}");
          return;
        },
      };

      match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
        Ok(task) => match task.await {
          Ok(()) => {
            let _ = workspace.update(cx, |workspace, cx| {
              if let Some(panel) = workspace
                .document_panels
                .iter()
                .find(|panel| panel.read(cx).id() == panel_id)
              {
                panel.update(cx, |panel, cx| panel.set_path(path, cx));
              }
              cx.notify();
            });
          },
          Err(error) => {
            let detail = error.to_string();
            let _ = window_handle.update(cx, |_, window, cx| {
              window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
            });
          },
        },
        Err(error) => {
          eprintln!("failed to access editor before save: {error}");
        },
      }
    })
    .detach();
  }

  fn prompt_save_active_flow_as(&mut self, editor: Entity<FlowEditor>, window: &mut Window, cx: &mut Context<Self>) {
    let Some(panel_id) = self.active_document_id else {
      return;
    };
    let save_path = cx.prompt_for_new_path(&default_save_directory(), Some(UNTITLED_FLOW_NAME));
    let window_handle = window.window_handle();
    cx.spawn(async move |workspace, cx| {
      let path = match save_path.await {
        Ok(Ok(Some(path))) => normalize_fl0_path(path),
        Ok(Ok(None)) => return,
        Ok(Err(error)) => {
          let detail = error.to_string();
          let _ = window_handle.update(cx, |_, window, cx| {
            window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
          });
          return;
        },
        Err(error) => {
          eprintln!("save dialog was canceled before completion: {error}");
          return;
        },
      };

      match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
        Ok(task) => match task.await {
          Ok(()) => {
            let _ = workspace.update(cx, |workspace, cx| {
              if let Some(panel) = workspace
                .flow_panels
                .iter()
                .find(|panel| panel.read(cx).id() == panel_id)
              {
                panel.update(cx, |panel, cx| panel.set_path(path, cx));
              }
              cx.notify();
            });
          },
          Err(error) => {
            let detail = error.to_string();
            let _ = window_handle.update(cx, |_, window, cx| {
              window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
            });
          },
        },
        Err(error) => {
          eprintln!("failed to access flow before save: {error}");
        },
      }
    })
    .detach();
  }
}

#[derive(Clone)]
enum PanelKind {
  Document {
    panel: Entity<DocumentPanel>,
    editor: Entity<RichTextEditor>,
  },
  Flow {
    panel: Entity<FlowPanel>,
    editor: Entity<FlowEditor>,
  },
}

enum PanelSaveOutcome {
  Saved,
  Cancelled,
  Failed(String),
}

#[hotpath::measure_all]
impl PanelKind {
  fn is_dirty(&self, cx: &App) -> bool {
    match self {
      PanelKind::Document { editor, .. } => editor.read(cx).has_unsaved_changes(),
      PanelKind::Flow { editor, .. } => editor.read(cx).has_unsaved_changes(),
    }
  }

  async fn save(&self, window_handle: AnyWindowHandle, cx: &mut gpui::AsyncApp) -> PanelSaveOutcome {
    match self {
      PanelKind::Document { panel, editor } => {
        let needs_save_as = match editor.update(cx, |editor, _| editor.document_path().is_none()) {
          Ok(needs_save_as) => needs_save_as,
          Err(error) => return PanelSaveOutcome::Failed(format!("failed to access editor before save: {error}")),
        };
        if needs_save_as {
          let path = match prompt_for_panel_save_path(window_handle, cx, UNTITLED_DOCUMENT_NAME).await {
            Ok(Some(path)) => normalize_db8_path(path),
            Ok(None) => return PanelSaveOutcome::Cancelled,
            Err(error) => return PanelSaveOutcome::Failed(error),
          };
          match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
            Ok(task) => match task.await {
              Ok(()) => {
                let _ = panel.update(cx, |panel, cx| panel.set_path(path, cx));
                PanelSaveOutcome::Saved
              },
              Err(error) => PanelSaveOutcome::Failed(error.to_string()),
            },
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access editor before save: {error}")),
          }
        } else {
          match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(task) => task
              .await
              .map(|_| PanelSaveOutcome::Saved)
              .unwrap_or_else(|error| PanelSaveOutcome::Failed(error.to_string())),
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access editor before save: {error}")),
          }
        }
      },
      PanelKind::Flow { panel, editor } => {
        let needs_save_as = match editor.update(cx, |editor, _| editor.document_path().is_none()) {
          Ok(needs_save_as) => needs_save_as,
          Err(error) => return PanelSaveOutcome::Failed(format!("failed to access flow before save: {error}")),
        };
        if needs_save_as {
          let path = match prompt_for_panel_save_path(window_handle, cx, UNTITLED_FLOW_NAME).await {
            Ok(Some(path)) => normalize_fl0_path(path),
            Ok(None) => return PanelSaveOutcome::Cancelled,
            Err(error) => return PanelSaveOutcome::Failed(error),
          };
          match editor.update(cx, |editor, cx| editor.save_as(path.clone(), cx)) {
            Ok(task) => match task.await {
              Ok(()) => {
                let _ = panel.update(cx, |panel, cx| panel.set_path(path, cx));
                PanelSaveOutcome::Saved
              },
              Err(error) => PanelSaveOutcome::Failed(error.to_string()),
            },
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access flow before save: {error}")),
          }
        } else {
          match editor.update(cx, |editor, cx| editor.save(cx)) {
            Ok(task) => task
              .await
              .map(|_| PanelSaveOutcome::Saved)
              .unwrap_or_else(|error| PanelSaveOutcome::Failed(error.to_string())),
            Err(error) => PanelSaveOutcome::Failed(format!("failed to access flow before save: {error}")),
          }
        }
      },
    }
  }

  fn discard(&self, cx: &mut gpui::AsyncApp) {
    match self {
      PanelKind::Document { editor, .. } => {
        let _ = editor.update(cx, |editor, cx| editor.discard_recovery_file(cx));
      },
      PanelKind::Flow { editor, .. } => {
        let _ = editor.update(cx, |editor, _| editor.discard_recovery_file());
      },
    }
  }
}

impl Workspace {
  fn close_collaboration_runtime(&mut self, cx: &mut Context<Self>) {
    self.close_collaboration_runtime_inner(cx, false);
  }

  fn close_collaboration_runtime_preserving_pending(&mut self, cx: &mut Context<Self>) {
    self.close_collaboration_runtime_inner(cx, true);
  }

  fn close_collaboration_runtime_inner(&mut self, cx: &mut Context<Self>, preserve_pending_updates: bool) {
    self.collaboration_runtime_id = self.collaboration_runtime_id.wrapping_add(1);
    if let Some(panel_id) = self.collaboration.panel_id {
      self.clear_collaboration_role_for_panel(panel_id, cx);
    }
    if let Some(host) = self.collaboration_host.take() {
      cx.spawn(async move |_, _| {
        let _ = host.shutdown().await;
      })
      .detach();
    }
    self.collaboration_client_updates = None;
    self.collaboration_last_published_hash = None;
    self.collaboration_delta_updates_since_checkpoint = 0;
    self.collaboration.pending_invite_copy_role = None;
    if !preserve_pending_updates {
      self.collaboration_pending_updates.clear();
    }
    self.collaboration.panel_id = None;
    self.collaboration.document_id = None;
    self.collaboration.format_kind = None;
    self.collaboration.peers.clear();
  }

  fn document_editor_for_panel(&self, panel_id: Uuid, cx: &App) -> Option<Entity<RichTextEditor>> {
    self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .map(|panel| panel.read(cx).editor())
  }

  fn flow_editor_for_panel(&self, panel_id: Uuid, cx: &App) -> Option<Entity<FlowEditor>> {
    self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
      .map(|panel| panel.read(cx).editor())
  }

  fn apply_collaboration_role_to_panel(&mut self, panel_id: Uuid, role: Role, cx: &mut Context<Self>) {
    let db8_role = match role {
      Role::Owner => Db8CollaborationRole::Owner,
      Role::Editor => Db8CollaborationRole::Editor,
      Role::Viewer => Db8CollaborationRole::Viewer,
    };
    if let Some(editor) = self.document_editor_for_panel(panel_id, cx) {
      editor.update(cx, |editor, cx| editor.set_collaboration_role(Some(db8_role), cx));
      return;
    }
    let flow_role = match role {
      Role::Owner => FlowCollaborationRole::Owner,
      Role::Editor => FlowCollaborationRole::Editor,
      Role::Viewer => FlowCollaborationRole::Viewer,
    };
    if let Some(editor) = self.flow_editor_for_panel(panel_id, cx) {
      editor.update(cx, |editor, cx| editor.set_collaboration_role(Some(flow_role), cx));
    }
  }

  fn clear_collaboration_role_for_panel(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    if let Some(editor) = self.document_editor_for_panel(panel_id, cx) {
      editor.update(cx, |editor, cx| editor.set_collaboration_role(None, cx));
      return;
    }
    if let Some(editor) = self.flow_editor_for_panel(panel_id, cx) {
      editor.update(cx, |editor, cx| editor.set_collaboration_role(None, cx));
    }
  }

  fn set_active_collaboration_panel(&mut self, panel_id: Uuid, cx: &mut Context<Self>) {
    if let Some(panel) = self
      .document_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      self.active_document_id = Some(panel_id);
      self.active_editor = Some(panel.read(cx).editor());
      self.active_flow = None;
      self.refresh_outline_tree(cx);
      cx.notify();
      return;
    }
    if let Some(panel) = self
      .flow_panels
      .iter()
      .find(|panel| panel.read(cx).id() == panel_id)
    {
      self.active_document_id = Some(panel_id);
      self.active_editor = None;
      self.active_flow = Some(panel.read(cx).editor());
      self.outline_cache = None;
      self.outline_viewport_paragraph = None;
      self.outline_scrolled_paragraph = None;
      cx.notify();
    }
  }

  fn spawn_host_update_loop(
    &mut self,
    panel_id: Uuid,
    document_state: SessionDocumentState,
    mut updates: broadcast::Receiver<LiveUpdate>,
    window_handle: AnyWindowHandle,
    cx: &mut Context<Self>,
  ) {
    cx.spawn(async move |workspace, cx| {
      loop {
        let update = match updates.recv().await {
          Ok(update) => update,
          Err(broadcast::error::RecvError::Lagged(_)) => continue,
          Err(broadcast::error::RecvError::Closed) => break,
        };
        match update.kind {
          LiveUpdateKind::Event(event) => {
            let _ = window_handle.update(cx, |_, _, cx| {
              let _ = workspace.update(cx, |workspace, cx| {
                workspace.apply_collaboration_session_event(panel_id, event, cx);
              });
            });
          },
          LiveUpdateKind::Wire(WireMessage::Update { application, .. }) if update.source_session_id.is_some() => {
            let source = match document_state.document.lock() {
              Ok(document) => document.clone(),
              Err(_) => {
                let _ = window_handle.update(cx, |_, _, cx| {
                  let _ = workspace.update(cx, |workspace, cx| {
                    if workspace.collaboration.panel_id == Some(panel_id) {
                      workspace.collaboration.state = SessionState::Failed;
                      workspace.collaboration.last_error = Some("Flowstate document state lock is poisoned".to_string());
                      cx.notify();
                    }
                  });
                });
                break;
              },
            };
            let _ = window_handle.update(cx, |_, _, cx| {
              let _ = workspace.update(cx, |workspace, cx| {
                if let Err(error) = workspace.apply_collaboration_source_to_panel(panel_id, source, application, cx) {
                  workspace.collaboration.last_error = Some(format!("{error:#}"));
                } else {
                  workspace.collaboration.last_error = None;
                }
                cx.notify();
              });
            });
          },
          LiveUpdateKind::Wire(_) => {},
        }
      }
    })
    .detach();
  }

  fn apply_collaboration_session_event(&mut self, panel_id: Uuid, event: SessionEvent, cx: &mut Context<Self>) {
    if self.collaboration.panel_id != Some(panel_id) {
      return;
    }
    match event {
      SessionEvent::PeerAuthorized { actor_id, session_id, role } | SessionEvent::PeerRoleChanged { actor_id, session_id, role } => {
        self
          .collaboration
          .peers
          .entry(session_id)
          .and_modify(|peer| {
            peer.actor_id = actor_id;
            peer.role = role;
          })
          .or_insert(CollaborationPeerInfo {
            actor_id,
            role,
            user_label: None,
            cursor: None,
            focus: None,
            viewport_hint: None,
            last_seen_millis: None,
          });
      },
      SessionEvent::PeerLeft { session_id, .. } => {
        self.collaboration.peers.remove(&session_id);
      },
      SessionEvent::StateChanged(state) => {
        self.collaboration.state = state;
      },
      SessionEvent::UpdateRejected { reason, .. }
      | SessionEvent::AssetTransferFailed { reason, .. }
      | SessionEvent::FatalError(reason)
      | SessionEvent::Error(reason) => {
        self.collaboration.last_error = Some(reason);
      },
      SessionEvent::Presence(presence) => {
        self
          .collaboration
          .peers
          .entry(presence.session_id)
          .and_modify(|peer| {
            peer.actor_id = presence.actor_id;
            peer.role = presence.role;
            peer.user_label = if presence.user_label.is_empty() {
              None
            } else {
              Some(presence.user_label.clone())
            };
            peer.cursor = presence.cursor.clone();
            peer.focus = presence.focus.clone();
            peer.viewport_hint = presence.viewport_hint.clone();
            peer.last_seen_millis = Some(presence.monotonic_millis);
          })
          .or_insert(CollaborationPeerInfo {
            actor_id: presence.actor_id,
            role: presence.role,
            user_label: if presence.user_label.is_empty() {
              None
            } else {
              Some(presence.user_label)
            },
            cursor: presence.cursor,
            focus: presence.focus,
            viewport_hint: presence.viewport_hint,
            last_seen_millis: Some(presence.monotonic_millis),
          });
        self.refresh_db8_remote_carets(cx);
      },
      SessionEvent::SnapshotApplied { .. }
      | SessionEvent::UpdateApplied { .. }
      | SessionEvent::AssetReceived { .. }
      | SessionEvent::Reconnecting => {},
    }
    cx.notify();
  }

  fn publish_db8_presence(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    if self.collaboration.panel_id != Some(panel_id) || self.collaboration.format_kind != Some(FormatKind::Db8) {
      return;
    }
    let cursor = editor.read_with(cx, |editor: &RichTextEditor, _| db8_presence_cursor(editor));
    if let Some(sender) = &self.collaboration_client_updates {
      let _ = sender.send(PendingCollaborationUpdate::Presence { cursor });
      return;
    }
    if let Some(host) = self.collaboration_host.as_ref() {
      let _ = host.publish_presence("", cursor, None, None);
    }
  }

  fn refresh_db8_remote_carets(&mut self, cx: &mut Context<Self>) {
    if self.collaboration.format_kind != Some(FormatKind::Db8) {
      return;
    }
    let Some(panel_id) = self.collaboration.panel_id else {
      return;
    };
    let Some(editor) = self.document_editor_for_panel(panel_id, cx) else {
      return;
    };
    let remote_carets = self
      .collaboration
      .peers
      .iter()
      .filter_map(|(session_id, peer)| {
        let offset = parse_db8_presence_cursor(peer.cursor.as_deref()?)?;
        Some(ExternalCaret {
          offset,
          color_rgb: remote_caret_color(session_id),
        })
      })
      .collect();
    editor.update(cx, |editor, cx| editor.set_external_carets(remote_carets, cx));
  }

  fn apply_collaboration_source_to_panel(
    &mut self,
    panel_id: Uuid,
    source: CollabDocument,
    application: Option<UpdateApplication>,
    cx: &mut Context<Self>,
  ) -> anyhow::Result<()> {
    if self.collaboration.panel_id != Some(panel_id) {
      return Ok(());
    }
    if self
      .collaboration
      .document_id
      .is_some_and(|document_id| document_id != source.document_id())
    {
      anyhow::bail!("remote collaboration update targets a different document");
    }
    if self
      .collaboration
      .format_kind
      .is_some_and(|format_kind| format_kind != source.format_kind())
    {
      anyhow::bail!("remote collaboration update targets a different format");
    }
    self.collaboration_last_published_hash = source.projection_hash().ok();
    match source.format_kind() {
      FormatKind::Db8 => {
        let Some(editor) = self.document_editor_for_panel(panel_id, cx) else {
          anyhow::bail!("collaboration DB8 panel is no longer open");
        };
        if let Some(UpdateApplication::Db8CanonicalOperations(bytes)) = application.as_ref()
          && let Some(operations) = crate::rich_text_element::decode_canonical_operations(bytes)
        {
          editor.update(cx, |editor, cx| {
            editor.clear_collaboration_edit();
            editor.apply_remote_operations(&operations, cx);
            editor.clear_collaboration_edit();
          });
          return Ok(());
        }
        let JoinedWorkspaceDocument::Document(document) = collab_document_to_workspace_document(source)? else {
          anyhow::bail!("remote DB8 update materialized as a different document kind");
        };
        editor.update(cx, |editor, cx| {
          editor.clear_collaboration_edit();
          editor.replace_document_from_collaboration(*document, cx);
          editor.clear_collaboration_edit();
        });
      },
      FormatKind::Fl0 => {
        let Some(editor) = self.flow_editor_for_panel(panel_id, cx) else {
          anyhow::bail!("collaboration FL0 panel is no longer open");
        };
        if let Some(UpdateApplication::Fl0ActionBundle(bytes)) = application.as_ref()
          && let Ok(actions) = postcard::from_bytes::<Vec<flowstate_flow::Action>>(bytes)
        {
          editor.update(cx, |editor, cx| editor.apply_remote_actions_from_collaboration(&actions, cx));
          return Ok(());
        }
        let JoinedWorkspaceDocument::Flow(document) = collab_document_to_workspace_document(source)? else {
          anyhow::bail!("remote FL0 update materialized as a different document kind");
        };
        editor.update(cx, |editor, cx| {
          editor.replace_document_from_collaboration(document, cx);
        });
      },
    }
    Ok(())
  }

  fn publish_db8_collaboration_edit(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    if self.collaboration_host.is_none() && self.collaboration_client_updates.is_none() && !self.can_queue_collaboration_update(panel_id) {
      return;
    }
    if self.collaboration.panel_id != Some(panel_id) {
      return;
    }
    let application = editor
      .read(cx)
      .last_collaboration_operation_bytes()
      .map(UpdateApplication::Db8CanonicalOperations);
    if let Some(application) = application
      && self.publish_collaboration_application_update(application, "DB8")
    {
      self.schedule_db8_collaboration_checkpoint(panel_id, editor, cx);
      return;
    }
    let document = editor.read(cx).document().clone();
    let document_id = self
      .collaboration
      .document_id
      .unwrap_or(CollabDocumentId(panel_id));
    let source = db8_collaboration_source(&document, document_id).map(|(source, _)| source);
    self.publish_collaboration_source(source, "DB8", None);
    self.collaboration_delta_updates_since_checkpoint = 0;
  }

  fn schedule_db8_collaboration_checkpoint(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    self.collaboration_delta_updates_since_checkpoint = self
      .collaboration_delta_updates_since_checkpoint
      .saturating_add(1);
    if self.collaboration_delta_updates_since_checkpoint < Self::COLLABORATION_CHECKPOINT_DELTA_INTERVAL {
      return;
    }
    self.collaboration_delta_updates_since_checkpoint = 0;
    let Some(document_id) = self.collaboration.document_id else {
      return;
    };
    let runtime_id = self.collaboration_runtime_id;
    let document = editor.read(cx).document().clone();
    let build_source = cx
      .background_executor()
      .spawn(async move { db8_collaboration_source(&document, document_id).map(|(source, _)| source) });
    cx.spawn(async move |workspace, cx| {
      let source = build_source.await;
      workspace.update(cx, |workspace, cx| {
        if workspace.collaboration_runtime_id != runtime_id || workspace.collaboration.panel_id != Some(panel_id) {
          return;
        }
        workspace.publish_collaboration_source(source, "DB8 checkpoint", None);
        cx.notify();
      })
    })
    .detach();
  }

  fn publish_fl0_collaboration_edit(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    if self.collaboration_host.is_none() && self.collaboration_client_updates.is_none() && !self.can_queue_collaboration_update(panel_id) {
      return;
    }
    if self.collaboration.panel_id != Some(panel_id) {
      return;
    }
    let application = editor
      .read(cx)
      .last_collaboration_actions()
      .and_then(|actions| postcard::to_stdvec(actions).ok())
      .map(UpdateApplication::Fl0ActionBundle);
    if let Some(application) = application
      && self.publish_collaboration_application_update(application, "FL0")
    {
      self.schedule_fl0_collaboration_checkpoint(panel_id, editor, cx);
      return;
    }
    let document = editor.read(cx).document().clone();
    let source = flowstate_flow::fl0_bytes(&document)
      .ok()
      .and_then(|bytes| collaboration_document_from_native_bytes(bytes, FormatKind::Fl0));
    self.publish_collaboration_source(source, "FL0", None);
  }

  fn schedule_fl0_collaboration_checkpoint(&mut self, panel_id: Uuid, editor: Entity<FlowEditor>, cx: &mut Context<Self>) {
    self.collaboration_delta_updates_since_checkpoint = self
      .collaboration_delta_updates_since_checkpoint
      .saturating_add(1);
    if self.collaboration_delta_updates_since_checkpoint < Self::COLLABORATION_CHECKPOINT_DELTA_INTERVAL {
      return;
    }
    self.collaboration_delta_updates_since_checkpoint = 0;
    let runtime_id = self.collaboration_runtime_id;
    let document = editor.read(cx).document().clone();
    let build_source = cx.background_executor().spawn(async move {
      flowstate_flow::fl0_bytes(&document)
        .ok()
        .and_then(|bytes| collaboration_document_from_native_bytes(bytes, FormatKind::Fl0))
    });
    cx.spawn(async move |workspace, cx| {
      let source = build_source.await;
      workspace.update(cx, |workspace, cx| {
        if workspace.collaboration_runtime_id != runtime_id || workspace.collaboration.panel_id != Some(panel_id) {
          return;
        }
        workspace.publish_collaboration_source(source, "FL0 checkpoint", None);
        cx.notify();
      })
    })
    .detach();
  }

  fn publish_collaboration_application_update(&mut self, application: UpdateApplication, label: &str) -> bool {
    if let Some(sender) = &self.collaboration_client_updates {
      if let Err(error) = sender.send(PendingCollaborationUpdate::Application { application }) {
        self.collaboration_client_updates = None;
        let update = error.0;
        let dropped_oldest =
          push_bounded_pending_collaboration_update(&mut self.collaboration_pending_updates, update, Self::MAX_PENDING_COLLABORATION_UPDATES);
        if dropped_oldest {
          self.collaboration.last_error.replace(format!(
            "{label} collaboration update queue is full; dropped the oldest pending application update."
          ));
        }
      }
      return true;
    }
    if let Some(host) = self.collaboration_host.as_ref() {
      if let Err(error) = host.publish_application_update(application) {
        self.collaboration.last_error = Some(format!("{error:#}"));
      }
      return true;
    }
    if self.can_queue_collaboration_update_for_active_panel() {
      let dropped_oldest = push_bounded_pending_collaboration_update(
        &mut self.collaboration_pending_updates,
        PendingCollaborationUpdate::Application { application },
        Self::MAX_PENDING_COLLABORATION_UPDATES,
      );
      if dropped_oldest {
        self.collaboration.last_error.replace(format!(
          "{label} collaboration update queue is full; dropped the oldest pending application update."
        ));
      }
      return true;
    }
    false
  }
  fn publish_collaboration_source(&mut self, source: Option<CollabDocument>, label: &str, application: Option<UpdateApplication>) {
    let Some(source) = source else {
      self
        .collaboration
        .last_error
        .replace(format!("Failed to build {label} collaboration source from edited document."));
      return;
    };
    if Some(source.document_id()) != self.collaboration.document_id || Some(source.format_kind()) != self.collaboration.format_kind {
      self.collaboration.last_error = Some(format!("{label} edit does not belong to the active collaboration document."));
      return;
    }
    let hash = source.projection_hash().ok();
    if hash.is_some() && hash == self.collaboration_last_published_hash {
      return;
    }
    if let Some(sender) = &self.collaboration_client_updates {
      if let Err(error) = sender.send(PendingCollaborationUpdate::Source { source, application, hash }) {
        let update = error.0;
        self.collaboration_client_updates = None;
        match update {
          PendingCollaborationUpdate::Source { source, hash, .. } => self.queue_pending_collaboration_update(source, label, hash),
          update @ PendingCollaborationUpdate::Application { .. } => {
            let dropped_oldest = push_bounded_pending_collaboration_update(
              &mut self.collaboration_pending_updates,
              update,
              Self::MAX_PENDING_COLLABORATION_UPDATES,
            );
            if dropped_oldest {
              self.collaboration.last_error.replace(format!(
                "{label} collaboration update queue is full; dropped the oldest pending application update."
              ));
            }
          },
          PendingCollaborationUpdate::Presence { .. } => {},
        }
      } else {
        self.collaboration_last_published_hash = hash;
        self.collaboration_delta_updates_since_checkpoint = 0;
      }
      return;
    }
    if let Some(host) = self.collaboration_host.as_ref() {
      if let Err(error) = host.publish_update_from_source(&source, application) {
        self.collaboration.last_error = Some(format!("{error:#}"));
      } else {
        self.collaboration_last_published_hash = hash;
        self.collaboration_delta_updates_since_checkpoint = 0;
      }
      return;
    }
    if self.can_queue_collaboration_update_for_active_panel() {
      self.queue_pending_collaboration_update(source, label, hash);
    }
  }

  fn can_queue_collaboration_update(&self, panel_id: Uuid) -> bool {
    self.collaboration.panel_id == Some(panel_id) && self.can_queue_collaboration_update_for_active_panel()
  }

  fn can_queue_collaboration_update_for_active_panel(&self) -> bool {
    matches!(self.collaboration.state, SessionState::Hosting | SessionState::Reconnecting)
      && self.collaboration.role.is_some_and(|role| role != "Viewer")
  }

  fn queue_pending_collaboration_update(&mut self, source: CollabDocument, label: &str, hash: Option<[u8; 32]>) {
    let dropped_oldest = push_bounded_pending_collaboration_update(
      &mut self.collaboration_pending_updates,
      PendingCollaborationUpdate::Source {
        source,
        application: None,
        hash,
      },
      Self::MAX_PENDING_COLLABORATION_UPDATES,
    );
    if dropped_oldest {
      self.collaboration.last_error.replace(format!(
        "{label} collaboration update queue is full; dropped the oldest pending source replacement."
      ));
    }
    self.collaboration_last_published_hash = hash;
  }

  fn drain_pending_updates_to_sender(&mut self, sender: &mpsc::UnboundedSender<PendingCollaborationUpdate>) -> usize {
    let mut drained = 0;
    while let Some(update) = self.collaboration_pending_updates.pop_front() {
      let hash = update.hash();
      if let Err(error) = sender.send(update) {
        self.collaboration_pending_updates.push_front(error.0);
        self
          .collaboration
          .last_error
          .replace("Failed to replay queued collaboration updates after reconnect.".to_string());
        break;
      }
      self.collaboration_last_published_hash = hash;
      drained += 1;
    }
    drained
  }

  fn drain_pending_updates_to_host(&mut self) -> usize {
    let Some(host) = self.collaboration_host.as_ref() else {
      return 0;
    };
    let mut drained = 0;
    while let Some(update) = self.collaboration_pending_updates.pop_front() {
      let hash = update.hash();
      let result = match &update {
        PendingCollaborationUpdate::Source { source, application, .. } => host.publish_update_from_source(source, application.clone()),
        PendingCollaborationUpdate::Application { application } => host.publish_application_update(application.clone()),
        PendingCollaborationUpdate::Presence { cursor } => host.publish_presence("", cursor.clone(), None, None),
      };
      if let Err(error) = result {
        self.collaboration_pending_updates.push_front(update);
        self.collaboration.last_error = Some(format!("{error:#}"));
        break;
      }
      self.collaboration_last_published_hash = hash;
      drained += 1;
    }
    drained
  }
}

struct CollaborationHostInput {
  document: CollabDocument,
  assets: AssetStore,
  source_hash: Option<[u8; 32]>,
}

enum CollaborationHostSnapshot {
  Db8 {
    panel_id: Uuid,
    document: Box<Document>,
  },
  Fl0 {
    panel_id: Uuid,
    document: flowstate_flow::FlowDocument,
  },
}

impl CollaborationHostSnapshot {
  const fn panel_id(&self) -> Uuid {
    match self {
      Self::Db8 { panel_id, .. } | Self::Fl0 { panel_id, .. } => *panel_id,
    }
  }

  const fn document_id(&self) -> CollabDocumentId {
    CollabDocumentId(self.panel_id())
  }

  const fn format_kind(&self) -> FormatKind {
    match self {
      Self::Db8 { .. } => FormatKind::Db8,
      Self::Fl0 { .. } => FormatKind::Fl0,
    }
  }

  fn into_host_input(self) -> Option<CollaborationHostInput> {
    match self {
      Self::Db8 { panel_id, document } => {
        let (document, assets) = db8_collaboration_source(&document, CollabDocumentId(panel_id))?;
        Some(CollaborationHostInput {
          source_hash: document.projection_hash().ok(),
          assets,
          document,
        })
      },
      Self::Fl0 { panel_id: _, document } => {
        collaboration_document_from_native_bytes(flowstate_flow::fl0_bytes(&document).ok()?, FormatKind::Fl0).map(|document| {
          CollaborationHostInput {
            source_hash: document.projection_hash().ok(),
            assets: AssetStore::default(),
            document,
          }
        })
      },
    }
  }
}

enum JoinedWorkspaceDocument {
  Document(Box<Document>),
  Flow(flowstate_flow::FlowDocument),
}

impl Workspace {
  fn active_collaboration_host_snapshot(&self, cx: &App) -> Option<CollaborationHostSnapshot> {
    let panel_id = self.active_document_id?;
    if let Some(editor) = self.active_editor.as_ref() {
      return Some(CollaborationHostSnapshot::Db8 {
        panel_id,
        document: Box::new(editor.read(cx).document().clone()),
      });
    }
    let editor = self.active_flow.as_ref()?;
    Some(CollaborationHostSnapshot::Fl0 {
      panel_id,
      document: editor.read(cx).document().clone(),
    })
  }

  fn collaboration_host_snapshot_for_panel(&self, panel_id: Uuid, cx: &App) -> Option<CollaborationHostSnapshot> {
    if let Some(editor) = self.document_editor_for_panel(panel_id, cx) {
      return Some(CollaborationHostSnapshot::Db8 {
        panel_id,
        document: Box::new(editor.read(cx).document().clone()),
      });
    }
    let editor = self.flow_editor_for_panel(panel_id, cx)?;
    Some(CollaborationHostSnapshot::Fl0 {
      panel_id,
      document: editor.read(cx).document().clone(),
    })
  }
}

enum PendingCollaborationUpdate {
  Source {
    source: CollabDocument,
    application: Option<UpdateApplication>,
    hash: Option<[u8; 32]>,
  },
  Application {
    application: UpdateApplication,
  },
  Presence {
    cursor: Option<String>,
  },
}

impl PendingCollaborationUpdate {
  const fn hash(&self) -> Option<[u8; 32]> {
    match self {
      Self::Source { hash, .. } => *hash,
      Self::Application { .. } | Self::Presence { .. } => None,
    }
  }
}

fn push_bounded_pending_collaboration_update(
  queue: &mut VecDeque<PendingCollaborationUpdate>,
  update: PendingCollaborationUpdate,
  max_len: usize,
) -> bool {
  if max_len == 0 {
    queue.clear();
    return true;
  }
  let dropped_oldest = queue.len() >= max_len;
  if dropped_oldest {
    queue.pop_front();
  }
  queue.push_back(update);
  dropped_oldest
}

fn collaboration_document_from_native_bytes(bytes: Vec<u8>, format_kind: FormatKind) -> Option<CollabDocument> {
  let decoded = decode_native_file(&bytes, format_kind).ok()?;
  CollabDocument::from_snapshot(&decoded.snapshot, Some(format_kind), Some(decoded.manifest.document_id)).ok()
}

fn db8_collaboration_source(document: &Document, document_id: CollabDocumentId) -> Option<(CollabDocument, AssetStore)> {
  let created_by_actor = ActorId::new();
  let source = db8_collab_document_with_id(document, document_id, created_by_actor)
    .ok()?
    .into_inner();
  Some((source, db8_asset_store(document)))
}

fn db8_asset_store(document: &Document) -> AssetStore {
  let mut store = AssetStore::default();
  for asset in document.assets.assets.values() {
    store.insert_verified(asset.bytes.as_ref().clone());
  }
  store
}

fn collab_document_to_workspace_document(document: CollabDocument) -> anyhow::Result<JoinedWorkspaceDocument> {
  match document.format_kind() {
    FormatKind::Db8 => Ok(JoinedWorkspaceDocument::Document(Box::new(document_from_db8_collab_source(&document)?))),
    FormatKind::Fl0 => Ok(JoinedWorkspaceDocument::Flow(flowstate_flow::flow_document_from_collab_source(
      &document,
    )?)),
  }
}

fn collaboration_sync_role(role: CollaborationInviteRole) -> Role {
  match role {
    CollaborationInviteRole::Owner => Role::Owner,
    CollaborationInviteRole::Editor => Role::Editor,
    CollaborationInviteRole::Viewer => Role::Viewer,
  }
}

fn collaboration_role_label(role: CollaborationInviteRole) -> &'static str {
  match role {
    CollaborationInviteRole::Owner => "Owner",
    CollaborationInviteRole::Editor => "Editor",
    CollaborationInviteRole::Viewer => "Viewer",
  }
}

fn collaboration_sync_role_label(role: impl std::fmt::Debug) -> &'static str {
  match format!("{role:?}").as_str() {
    "Owner" => "Owner",
    "Editor" => "Editor",
    "Viewer" => "Viewer",
    _ => "Unknown",
  }
}

fn collaboration_peer_display_name(peer: &CollaborationPeerInfo) -> String {
  peer
    .user_label
    .as_deref()
    .filter(|label| !label.trim().is_empty())
    .map(|label| label.trim().to_string())
    .unwrap_or_else(|| format!("Peer {}", collaboration_short_uuid(peer.actor_id.0)))
}

fn collaboration_short_uuid(id: Uuid) -> String {
  id.to_string().chars().take(8).collect()
}

fn show_collaboration_prompt(window: &mut Window, cx: &mut Context<Workspace>, level: PromptLevel, message: &'static str, detail: &str) {
  std::mem::drop(window.prompt(level, message, Some(detail), &[PromptButton::ok("Ok")], cx));
}

#[hotpath::measure]
async fn prompt_for_panel_save_path(
  window_handle: AnyWindowHandle,
  cx: &mut gpui::AsyncApp,
  suggested_name: &'static str,
) -> Result<Option<PathBuf>, String> {
  let save_path = window_handle
    .update(cx, |_, _, cx| cx.prompt_for_new_path(&default_save_directory(), Some(suggested_name)))
    .map_err(|error| format!("failed to open save dialog: {error}"))?;
  match save_path.await {
    Ok(Ok(path)) => Ok(path),
    Ok(Err(error)) => Err(error.to_string()),
    Err(error) => Err(format!("save dialog closed unexpectedly: {error}")),
  }
}

#[hotpath::measure]
fn show_save_failed(window_handle: AnyWindowHandle, cx: &mut gpui::AsyncApp, detail: String) {
  let _ = window_handle.update(cx, |_, window, cx| {
    window.prompt(PromptLevel::Critical, "Save failed", Some(&detail), &[PromptButton::ok("Ok")], cx)
  });
}
fn db8_presence_cursor(editor: &RichTextEditor) -> Option<String> {
  let selection = editor.selection();
  (selection.anchor == selection.head).then(|| format!("db8:{}:{}", selection.head.paragraph, selection.head.byte))
}

fn parse_db8_presence_cursor(cursor: &str) -> Option<DocumentOffset> {
  let mut parts = cursor.split(':');
  if parts.next()? != "db8" {
    return None;
  }
  let paragraph = parts.next()?.parse().ok()?;
  let byte = parts.next()?.parse().ok()?;
  if parts.next().is_some() {
    return None;
  }
  Some(DocumentOffset { paragraph, byte })
}

fn remote_caret_color(session_id: &SessionId) -> u32 {
  let bytes = session_id.0.as_bytes();
  let hue = bytes
    .iter()
    .fold(0u32, |acc, byte| acc.wrapping_mul(31).wrapping_add(u32::from(*byte)))
    % 360;
  hsl_to_rgb(hue as f32, 0.72, 0.42)
}

fn hsl_to_rgb(hue: f32, saturation: f32, lightness: f32) -> u32 {
  let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
  let hue_prime = hue / 60.0;
  let x = chroma * (1.0 - (hue_prime % 2.0 - 1.0).abs());
  let (r1, g1, b1) = match hue_prime as u32 {
    0 => (chroma, x, 0.0),
    1 => (x, chroma, 0.0),
    2 => (0.0, chroma, x),
    3 => (0.0, x, chroma),
    4 => (x, 0.0, chroma),
    _ => (chroma, 0.0, x),
  };
  let m = lightness - chroma / 2.0;
  let channel = |value: f32| ((value + m).clamp(0.0, 1.0) * 255.0).round() as u32;
  (channel(r1) << 16) | (channel(g1) << 8) | channel(b1)
}

enum LoadedWorkspaceDocument {
  Document {
    document: Box<Document>,
    path: Option<PathBuf>,
    title: Option<String>,
  },
  Flow {
    document: flowstate_flow::FlowDocument,
    path: PathBuf,
  },
}

#[hotpath::measure]
fn load_workspace_document(path: PathBuf) -> Result<LoadedWorkspaceDocument, String> {
  if is_flow_path(&path) {
    return Ok(LoadedWorkspaceDocument::Flow {
      document: flowstate_flow::load_flow_document_or_new(&path),
      path,
    });
  }
  load_document_for_open(&path)
    .map(|loaded| LoadedWorkspaceDocument::Document {
      document: Box::new(loaded.document),
      path: loaded.path,
      title: loaded.title,
    })
    .map_err(|error| error.to_string())
}

#[hotpath::measure]
fn is_flow_path(path: &Path) -> bool {
  path
    .extension()
    .and_then(|extension| extension.to_str())
    .is_some_and(|extension| extension.eq_ignore_ascii_case("fl0"))
}
