//! Terminal editor for the pool settings of this workspace on this machine.
use super::{Profiles, State};
use crate::{
    remote::pool::{self, WorkerArgs, settings::PoolSettings},
    storage::document::Document,
};
use roomkit::RoomKit;
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{
    Choice, Command, Event, FieldKind, FormField, FormSpec, FormValues, Request,
};
use roomkit_tui_engine::{CommandResult, SessionConfig};
mod live;

pub(super) struct Pending {
    room: String,
    id: String,
    document: Document<PoolSettings>,
}

impl Profiles {
    pub(crate) async fn executors(
        &self,
        kit: &RoomKit<SqliteStore>,
        request: Request,
        current: &SessionConfig,
    ) -> Result<CommandResult, String> {
        match request.command {
            Command::Invoke { arguments, .. } if arguments.trim() == "configure" => {
                self.open_executor_form(current).await
            }
            Command::Invoke { arguments, .. } => {
                pool::panel(self.active_pool().as_deref(), current, arguments.trim()).await
            }
            Command::SubmitForm { form, values, .. } => {
                self.save_executor_form(kit, current, &form, &values).await
            }
            _ => Err("Unsupported executor request".into()),
        }
    }
    async fn open_executor_form(&self, current: &SessionConfig) -> Result<CommandResult, String> {
        let document = PoolSettings::open(&self.state_dir, &self.workspace)?;
        let mut state = self.state.lock().await;
        let available = state
            .agents()
            .into_iter()
            .map(|(agent, _)| agent.id)
            .collect::<Vec<_>>();
        let id = uuid::Uuid::new_v4().to_string();
        let mut effective = document.value.clone();
        if let Some(active) = self.active_pool() {
            effective.schedule = true;
            effective.group = Some(active.group().to_owned());
        }
        let form = editor(&id, &effective, &available);
        state.executor_form = Some(Pending {
            room: current.room.to_string(),
            id,
            document,
        });
        Ok(CommandResult::Events(vec![Event::Form {
            conversation: current.room.as_str().into(),
            form,
        }]))
    }
    async fn save_executor_form(
        &self,
        kit: &RoomKit<SqliteStore>,
        current: &SessionConfig,
        form: &str,
        values: &FormValues,
    ) -> Result<CommandResult, String> {
        let mut state = self.state.lock().await;
        let pending = state
            .executor_form
            .as_ref()
            .filter(|p| p.id == form && p.room == current.room.as_str())
            .ok_or("Reopen /executors configure before saving")?;
        let saved = submitted(&pending.document.value, values)?;
        validate_profiles(&state, &saved)?;
        let change = live::Change::prepare(self, &state, &saved).await?;
        let notice = live::save(self, kit, &mut state, saved, change).await?;
        Ok(CommandResult::Events(vec![Event::Notice {
            conversation: current.room.as_str().into(),
            text: notice.into(),
        }]))
    }
}

fn validate_profiles(state: &State, saved: &PoolSettings) -> Result<(), String> {
    if saved.schedule && state.agent(crate::remote::AGENT).is_some() {
        return Err(format!(
            "Rename the local agent {} before enabling the pool",
            crate::remote::AGENT
        ));
    }
    for id in &saved.worker.worker_profile {
        if state.agent(id).is_none() {
            return Err(format!(
                "Unknown local profile {id}; configure it in /agents"
            ));
        }
    }
    Ok(())
}

fn text(name: &str, title: &str, value: &str, help: &str) -> FormField {
    FormField::new(name, title, FieldKind::Text)
        .with_value(value)
        .with_help(help)
}

fn editor(id: &str, saved: &PoolSettings, profiles: &[String]) -> FormSpec {
    let worker = &saved.worker;
    let url_help = if saved.url.is_some() {
        "A private URL is saved. Leave blank to keep it, enter - to forget it. ROOMKIT_EXECUTOR_REDIS_URL takes priority."
    } else {
        "Optional redis:// or rediss:// URL, stored privately. ROOMKIT_EXECUTOR_REDIS_URL takes priority."
    };
    let fields = vec![
        FormField::new(
            "schedule",
            "Schedule remote tasks",
            FieldKind::Choice {
                choices: vec![Choice::new("off", "Off"), Choice::new("on", "On")],
            },
        )
        .with_value(if saved.schedule { "on" } else { "off" }),
        text(
            "group",
            "Pool group",
            saved.group.as_deref().unwrap_or(""),
            "Use the same group on the coordinator and its workers.",
        ),
        FormField::new("url", "Redis connection URL", FieldKind::Secret).with_help(url_help),
        text(
            "name",
            "Local worker name",
            worker.configured_name(),
            "Blank uses the hostname. Start this worker with roomkit-code --executor.",
        ),
        text(
            "slots",
            "Concurrent worker slots",
            &worker.slots().to_string(),
            "1–256 simultaneous tasks.",
        ),
        text(
            "labels",
            "Placement labels",
            &worker.label_text(),
            "Comma-separated key=value pairs, for example role=review.",
        ),
        text(
            "profiles",
            "Local agent profiles",
            &worker.worker_profile.join(", "),
            &format!(
                "Comma-separated IDs, first is default. Blank serves the selected agent. Available here: {}",
                profiles.join(", ")
            ),
        ),
        text(
            "host",
            "Physical host OS",
            worker.host_os().unwrap_or(""),
            "Optional when running in a container or VM. The worker detects its execution OS.",
        ),
    ];
    FormSpec::new(
        id,
        "executors",
        "Executor settings · this workspace",
        fields,
    )
}

fn submitted(previous: &PoolSettings, values: &FormValues) -> Result<PoolSettings, String> {
    let field = |name| {
        values
            .get(name)
            .map(str::trim)
            .ok_or_else(|| format!("Missing executor field {name}"))
    };
    let schedule = match field("schedule")? {
        "on" => true,
        "off" => false,
        _ => return Err("Choose On or Off for scheduling".into()),
    };
    let group = field("group")?;
    let url = match values.get("url").unwrap_or("").trim() {
        "" => previous.url.clone(),
        "-" => None,
        url => Some(url.to_owned()),
    };
    let saved = PoolSettings {
        group: (!group.is_empty()).then(|| group.to_owned()),
        schedule,
        url,
        worker: WorkerArgs::from_fields(
            field("name")?,
            field("slots")?,
            field("labels")?,
            field("profiles")?,
            field("host")?,
        )?,
    };
    saved.validate()?;
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> FormValues {
        [
            ("schedule", "off"),
            ("group", ""),
            ("url", ""),
            ("name", ""),
            ("slots", "1"),
            ("labels", ""),
            ("profiles", ""),
            ("host", ""),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn private_endpoint_is_masked_retained_replaced_and_cleared() -> Result<(), String> {
        let mut values = fields();
        let original = "redis://:fixture-password@localhost/";
        values.insert("url", original);
        let saved = submitted(&PoolSettings::default(), &values)?;
        let form = editor("id", &saved, &[]);
        let url = form
            .fields
            .iter()
            .find(|field| field.name == "url")
            .ok_or("missing URL")?;
        assert!(matches!(url.kind, FieldKind::Secret) && url.value.is_empty());
        assert!(
            !serde_json::to_string(&form)
                .map_err(|e| e.to_string())?
                .contains("fixture-password")
        );
        values.insert("url", "");
        assert_eq!(submitted(&saved, &values)?.url.as_deref(), Some(original));
        values.insert("url", "-");
        assert!(submitted(&saved, &values)?.url.is_none());
        values.insert("url", "redis://localhost:6380/");
        assert_eq!(
            submitted(&saved, &values)?.url.as_deref(),
            Some("redis://localhost:6380/")
        );
        Ok(())
    }

    #[test]
    fn invalid_submission_cannot_enable_scheduling_or_advertise_invalid_resources() {
        for (key, value) in [
            ("schedule", "on"),
            ("slots", "0"),
            ("slots", "257"),
            ("url", "https://bad.invalid"),
            ("labels", "role=qa,role=review"),
            ("profiles", "reader, reader"),
            ("group", "not/a/group"),
        ] {
            let mut values = fields();
            values.insert(key, value);
            assert!(
                submitted(&PoolSettings::default(), &values).is_err(),
                "{key}"
            );
        }
    }
}
