use super::*;
use roomkit_cli_roomkit::{CommandResult, SessionConfig};
use roomkit_tui::protocol::{
    Choice, Event, PanelSpec, SidebarAction, SidebarContent, SidebarEntry, SidebarSection,
    SidebarTone,
};

fn panel(body: String) -> PanelSpec {
    PanelSpec {
        id: uuid::Uuid::new_v4().to_string(),
        command: "update".into(),
        title: "RoomKit Code · updates".into(),
        body,
        actions: vec![],
        dismiss: "dismiss".into(),
    }
}
impl Updates {
    pub fn events(&self, view: &SessionConfig) -> Vec<Event> {
        let mut events = vec![Event::ApplicationInfo {
            conversation: view.room.as_str().into(),
            text: Some(self.label()),
        }];
        if let Some((room, panel)) = &self.state().panel
            && room == view.room.as_str()
        {
            events.push(Event::PanelUpdated {
                conversation: room.as_str().into(),
                panel: panel.clone(),
            });
        }
        events
    }
    pub fn sidebar(&self) -> SidebarSection {
        SidebarSection {
            title: "Application".into(),
            action: None,
            content: SidebarContent::Entries(vec![SidebarEntry {
                label: self.label(),
                detail: String::new(),
                tone: SidebarTone::Muted,
                action: Some(SidebarAction {
                    command: "update".into(),
                    arguments: String::new(),
                }),
            }]),
        }
    }
    fn finish(&self, room: &str, mut panel: PanelSpec, result: Result<Option<Action>>) {
        match result {
            Ok(Some(action)) => {
                let token = uuid::Uuid::new_v4().to_string();
                let label = match &action {
                    Action::Install(r) => format!("Install {}", r.tag_name),
                    Action::Rollback(v) => format!("Restore v{v}"),
                };
                panel.actions = vec![Choice {
                    value: format!("confirm {token}"),
                    label,
                    description: "Takes effect on next launch; current conversation continues"
                        .into(),
                }];
                self.state().pending = Some((room.into(), token, action));
            }
            Ok(None) => {}
            Err(e) => {
                self.record_failure(e.as_ref());
                panel.body = format!(
                    "Could not complete this operation.\n\n{e}\n\nYour current installation remains available. Use /update to try again."
                );
            }
        }
        self.state().panel = Some((room.into(), panel));
        self.changes.send_replace(());
    }
    pub fn handle(
        self: &Arc<Self>,
        view: &SessionConfig,
        argument: &str,
    ) -> std::result::Result<CommandResult, String> {
        let room = view.room.to_string();
        if argument == "dismiss" {
            let mut state = self.state();
            if state.pending.as_ref().is_some_and(|p| p.0 == room) {
                state.pending = None;
            }
            state.panel = None;
            return Ok(CommandResult::Events(vec![]));
        }
        let Some(installation) = self.installation.clone() else {
            return Ok(CommandResult::Events(vec![Event::Panel {
                conversation: room.as_str().into(),
                panel: panel(format!(
                    "Version {CURRENT}\n\nUpdates are managed by the method you used to install RoomKit Code."
                )),
            }]));
        };
        installation
            .ensure_current(&self.exe)
            .map_err(|e| e.to_string())?;
        let guard =
            self.busy.clone().try_lock_owned().map_err(
                |_| "An update check or installation is already running; try again shortly",
            )?;
        let action = if let Some(token) = argument.strip_prefix("confirm ") {
            let mut state = self.state();
            if !state
                .pending
                .as_ref()
                .is_some_and(|p| p.0 == room && p.1 == token)
            {
                return Err("This update confirmation expired; open /update again".into());
            }
            state.pending.take().map(|p| p.2)
        } else {
            if !matches!(argument, "" | "check" | "rollback") {
                return Err("Use /update or /update rollback".into());
            }
            self.state().pending = None;
            None
        };
        let mut opening = panel(
            if action.is_some() {
                "Installing the selected version…"
            } else if argument == "rollback" {
                "Inspecting the previous version…"
            } else {
                "Checking for updates…"
            }
            .into(),
        );
        self.state().panel = Some((room.clone(), opening.clone()));
        let this = self.clone();
        let rollback = argument == "rollback";
        let response = CommandResult::Events(vec![Event::Panel {
            conversation: room.as_str().into(),
            panel: opening.clone(),
        }]);
        self.spawn(async move {
            let _guard = guard;
            let operation = async {
                match action {
                    Some(Action::Install(release)) => {
                        let (file, checksum) = GitHub::new().await?.download(&release).await?;
                        // The short atomic commit is synchronous and cannot be interrupted halfway by task cancellation.
                        installation.activate(file.path(), release.number(), &checksum, Some(&this.exe))?;
                        opening.body = format!("{} is installed.\n\nYour conversation continues with v{CURRENT}. Exit and launch RoomKit Code again to use the new version.\n\nThe previous binary is retained; /update rollback restores it after restarting. Application data is unchanged.", release.tag_name);
                        this.state().notice = Some(format!("{} installed · restart to use", release.tag_name));
                        Ok(None)
                    }
                    Some(Action::Rollback(previous)) => {
                        installation.rollback(&this.exe, &previous)?;
                        opening.body = format!("v{previous} restored. Exit and launch RoomKit Code again to use it.\n\nApplication data is unchanged. Rollback restores the binary; it does not downgrade a future incompatible database format.");
                        this.state().notice = Some(format!("v{previous} restored · restart to use"));
                        Ok(None)
                    }
                    None if rollback => {
                        let previous = installation.previous()?;
                        opening.body = format!("Current: v{CURRENT}\nPrevious: v{previous}\n\nRestore the previous binary? Your current conversation will continue; restart afterward.\n\nApplication data is preserved. Binary rollback cannot undo incompatible data migrations introduced by future releases.");
                        Ok(Some(Action::Rollback(previous)))
                    }
                    None => {
                        let release = GitHub::new().await?.latest().await?;
                        this.save_cache(release.clone());
                        this.state().release = release.clone();
                        let Some(release) = release else {
                            opening.body = NO_UPDATE.into();
                            return Ok(None);
                        };
                        if version(release.number())? <= version(CURRENT)? {
                            opening.body = NO_UPDATE.into();
                            Ok(None)
                        } else {
                            opening.body = format!("Current: v{CURRENT}\nAvailable: {}\nPlatform: {TARGET}\nDownload: {} bytes\nDestination: {}\n\nThe binary will be verified with the release SHA-256 checksum.\nYour conversation will continue; the new version takes effect on next launch.\n\nRelease notes: https://github.com/roomkit-live/roomkit-code/releases/tag/{}", release.tag_name, release.asset(false)?.size, installation.bin.display(), release.tag_name);
                            Ok(Some(Action::Install(release)))
                        }
                    }
                }
            };
            let result = tokio::select! { result = operation => result, () = this.stop.cancelled() => return };
            this.finish(&room, opening, result);
        });
        Ok(response)
    }
}
