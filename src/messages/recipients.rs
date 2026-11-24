//! Catalogue shared by the slash command and the first-argument selector.
use super::directory::contacts;
use crate::sessions::Sessions;
use roomkit_tui::protocol::{Choice, Event, RequestId};
use roomkit_tui_engine::{CommandResult, SessionConfig};

#[derive(Default)]
struct Catalogue {
    choices: Vec<Choice>,
    warnings: Vec<String>,
}

impl Catalogue {
    async fn load(app: &Sessions, view: &SessionConfig) -> Self {
        let mut result = Self::default();
        let (sessions, workers) =
            tokio::join!(contacts(app, view, false), executor_choices(app, view));
        let sessions = match sessions {
            Ok(all) => Ok(all),
            Err(err) => {
                result.warnings.push(err);
                contacts(app, view, true).await
            }
        };
        match sessions {
            Ok(cs) => result.choices.extend(
                cs.into_iter()
                    .map(|c| Choice::new(c.key(), &c.name).with_description(c.description())),
            ),
            Err(err) if !result.warnings.contains(&err) => result.warnings.push(err),
            Err(_) => {}
        }
        result.choices.extend(workers.choices);
        result.warnings.extend(workers.warnings);
        result
    }

    fn finish(
        self,
        view: &SessionConfig,
        event: impl FnOnce(Vec<Choice>) -> Event,
    ) -> Result<CommandResult, String> {
        if self.choices.is_empty() && !self.warnings.is_empty() {
            return Err(self.warnings.join(" · "));
        }
        let mut events = Vec::new();
        if !self.warnings.is_empty() {
            events.push(Event::Notice {
                conversation: view.room.as_str().into(),
                text: self.warnings.join(" · "),
            });
        }
        events.push(event(self.choices));
        Ok(CommandResult::Events(events))
    }
}

async fn executor_choices(app: &Sessions, view: &SessionConfig) -> Catalogue {
    let mut result = Catalogue::default();
    if let Some(pool) = app.profiles.active_pool() {
        result.choices = app
            .profiles
            .resources
            .tasks
            .of(view.room.as_str())
            .into_iter()
            .filter(|(_, task)| task.running())
            .filter_map(|(_, task)| pool.task_choice(&task))
            .collect();
        match pool.profile_choices().await {
            Ok(cs) => result.choices.extend(cs),
            Err(err) => result
                .warnings
                .push(format!("Executor profiles unavailable: {err}")),
        }
    }
    result
}

pub(super) async fn suggest(
    app: &Sessions,
    view: &SessionConfig,
    request: RequestId,
) -> Result<CommandResult, String> {
    Catalogue::load(app, view)
        .await
        .finish(view, |choices| Event::InputChoices {
            conversation: view.room.as_str().into(),
            request,
            choices,
        })
}

pub(super) async fn picker(
    app: &Sessions,
    view: &SessionConfig,
    query: &str,
) -> Result<CommandResult, String> {
    let mut catalogue = Catalogue::load(app, view).await;
    let query = query.to_lowercase();
    catalogue.choices.retain(|c| {
        [&c.label, &c.value, &c.description]
            .iter()
            .any(|s| s.to_lowercase().contains(&query))
    });
    catalogue.finish(view, |choices| Event::Choices {
        conversation: view.room.as_str().into(),
        command: "message".into(),
        title: "Choose a recipient".into(),
        choices,
    })
}
