//! Assemble optional consumers from CLI facts; product paths and policy stay here.

use crate::{
    config::Settings,
    herdr::{self, Report, SessionRef, State},
};
use roomkit_tui::{SessionObservation, SessionObserver};

pub(super) struct Integration {
    reporter: Option<herdr::Reporter>,
}

impl Integration {
    /// Report the observed view to Herdr when its endpoint is detected.
    pub fn start(
        settings: &Settings,
        diagnostics: std::sync::Arc<crate::diagnostics::Diagnostics>,
        observer: &SessionObserver,
        tasks: std::sync::Arc<crate::delegation::Tasks>,
    ) -> Self {
        let endpoint = settings
            .herdr
            .then(|| herdr::Endpoint::detect(crate::config::default_state_dir()))
            .flatten();
        let Some(endpoint) = endpoint else {
            return Self { reporter: None };
        };
        let source = observer.clone();
        let resume = Resume::new(settings);
        let reporter = herdr::Reporter::start(
            endpoint,
            move || {
                source.latest().map(|mut state| {
                    // A view can be in a task's room: the session is its
                    // conversation, the one a resume command reopens.
                    state.conversation = tasks.root_of(state.conversation.as_str()).into();
                    resume.report(state)
                })
            },
            move |code| {
                diagnostics.record("herdr_integration", serde_json::json!({"code":code}), None)
            },
        );
        Self {
            reporter: Some(reporter),
        }
    }

    pub fn freeze(&self) {
        if let Some(reporter) = &self.reporter {
            reporter.freeze();
        }
    }
    pub async fn close(self) {
        if let Some(reporter) = self.reporter {
            reporter.close().await;
        }
    }
}

/// The command line that reopens a conversation of this project with the
/// launch's explicit options. Herdr and the exit summary share it.
pub(super) struct Resume {
    prefix: Option<Vec<String>>,
    options: Option<Vec<String>>,
}

impl Resume {
    pub(super) fn new(settings: &Settings) -> Self {
        let prefix = settings
            .workspace
            .to_str()
            .zip(settings.state_dir.to_str())
            .map(|(workspace, storage)| {
                ["roomkit-code", workspace, "--state-dir", storage]
                    .map(String::from)
                    .to_vec()
            });
        Self {
            prefix,
            options: settings.resume_options.clone(),
        }
    }

    /// Arguments reopening `conversation` in `mode`, or None when they cannot
    /// be expressed safely (unknown mode, non-UTF-8 path, quote in a path).
    pub(super) fn command(&self, conversation: &str, mode: &str) -> Option<Vec<String>> {
        if !matches!(mode, "ask" | "plan" | "yolo") {
            return None;
        }
        let mut args = self.prefix.clone()?;
        args.extend([
            "--session".into(),
            conversation.into(),
            "--mode".into(),
            mode.into(),
        ]);
        args.extend(self.options.clone()?);
        herdr::valid_resume(&args).then_some(args)
    }

    fn report(&self, observation: SessionObservation) -> Report {
        let resume = observation.mode.as_ref().and_then(|mode| {
            self.command(observation.conversation.as_str(), mode.selected.as_str())
        });
        let state = if !observation.permissions.is_empty() {
            State::Blocked
        } else if observation
            .operation
            .as_ref()
            .is_some_and(|operation| operation.outcome.is_none())
            || observation
                .run
                .as_ref()
                .is_some_and(|run| run.outcome.is_none())
        {
            State::Working
        } else {
            State::Idle
        };
        Report {
            state,
            title: observation.title,
            session: SessionRef {
                id: observation.conversation.into_inner(),
                resume,
            },
        }
    }
}

#[cfg(test)]
mod tests;
