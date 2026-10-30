//! Assemble optional consumers from CLI facts; product paths and policy stay here.

use crate::{
    config::Settings,
    herdr::{self, Report, SessionRef, State},
};
use roomkit_tui::{SessionObservation, SessionObserver};

pub(super) struct Integration {
    pub observer: Option<SessionObserver>,
    reporter: Option<herdr::Reporter>,
}

impl Integration {
    pub fn start(
        settings: &Settings,
        diagnostics: std::sync::Arc<crate::diagnostics::Diagnostics>,
    ) -> Self {
        let endpoint = settings
            .herdr
            .then(|| herdr::Endpoint::detect(crate::config::default_state_dir()))
            .flatten();
        let Some(endpoint) = endpoint else {
            return Self {
                observer: None,
                reporter: None,
            };
        };
        let observer = SessionObserver::default();
        let source = observer.clone();
        let resume = Resume::new(settings);
        let reporter = herdr::Reporter::start(
            endpoint,
            move || source.latest().map(|state| resume.report(state)),
            move |code| {
                diagnostics.record("herdr_integration", serde_json::json!({"code":code}), None)
            },
        );
        Self {
            observer: Some(observer),
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

struct Resume {
    prefix: Option<Vec<String>>,
    options: Option<Vec<String>>,
}

impl Resume {
    fn new(settings: &Settings) -> Self {
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

    fn report(&self, observation: SessionObservation) -> Report {
        let resume = (|| {
            let mut args = self.prefix.clone()?;
            let mode = observation.mode.as_ref()?.selected.as_str();
            if !matches!(mode, "ask" | "yolo") {
                return None;
            }
            args.extend([
                "--session".into(),
                observation.conversation.0.clone(),
                "--mode".into(),
                mode.into(),
            ]);
            args.extend(self.options.clone()?);
            herdr::valid_resume(&args).then_some(args)
        })();
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
                id: observation.conversation.0,
                resume,
            },
        }
    }
}

#[cfg(test)]
mod tests;
