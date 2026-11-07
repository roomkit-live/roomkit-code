//! What an unattended launch says to the host, and when it is done, decided
//! from the host's events alone: the transport, the signals and the output
//! stay with the caller.

use std::collections::BTreeSet;

use roomkit_tui::protocol::{
    ActorId, Command, ConversationId, Decision, Event, OperationId, Request, RequestId, RunId,
    RunOutcome, Snapshot,
};

/// The operation the launch's message is sent under.
const MESSAGE: &str = "headless-message";

/// What the launch does in answer to an event or a signal.
#[derive(Debug, PartialEq)]
pub(super) enum Step {
    /// Send this request to the host.
    Send(Request),
    /// Tell the person who started the launch, apart from the answer.
    Tell(String),
}

/// How the launch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ending {
    /// The agent's turn ended with its answer.
    Completed,
    /// The message was refused, or a turn ended without its answer: on its
    /// round limit, an error, or a cut nobody asked for.
    Failed,
    /// A signal stopped the launch.
    Interrupted,
}

/// The conversation as the launch knows it.
pub(super) struct Driver {
    /// The message, until the first snapshot names where to send it.
    message: Option<String>,
    conversation: Option<ConversationId>,
    agent: Option<ActorId>,
    requests: u64,
    /// The host refused the message.
    refused: bool,
    /// How the message's operation ended, once it has.
    finished: Option<RunOutcome>,
    /// Runs started and not yet finished.
    live: BTreeSet<RunId>,
    /// A run ended otherwise than completed.
    failed: bool,
    interrupted: bool,
    /// The agent's latest finished message.
    answer: Option<String>,
}

impl Driver {
    pub fn new(message: String) -> Self {
        Self {
            message: Some(message),
            conversation: None,
            agent: None,
            requests: 0,
            refused: false,
            finished: None,
            live: BTreeSet::new(),
            failed: false,
            interrupted: false,
            answer: None,
        }
    }

    /// The steps `event` calls for.
    pub fn on_event(&mut self, event: &Event) -> Vec<Step> {
        match event {
            Event::Snapshot(snapshot) => self.submit(snapshot),
            Event::Reply {
                request,
                error: Some(rejection),
            } => {
                // Request 1 is the message; later ones are decisions and cuts.
                self.refused |= request.get() == 1;
                vec![Step::Tell(format!("Refused: {}", rejection.message))]
            }
            Event::RunStarted { run, .. } => {
                self.live.insert(run.clone());
                // A signal that came before the run started stops it now.
                if self.interrupted {
                    return self.cut(run.clone()).into_iter().collect();
                }
                Vec::new()
            }
            Event::RunFinished { run, outcome, .. } => {
                self.live.remove(run);
                self.failed |= *outcome != RunOutcome::Completed;
                Vec::new()
            }
            Event::OperationFinished {
                operation, outcome, ..
            } if operation.as_str() == MESSAGE => {
                self.finished = Some(*outcome);
                self.failed |= *outcome != RunOutcome::Completed;
                Vec::new()
            }
            Event::PermissionRequested { permission, .. } => {
                let decide = Command::Decide {
                    permission: permission.id.clone(),
                    decision: Decision::Deny,
                };
                let mut steps: Vec<Step> = self.request(decide).into_iter().collect();
                steps.push(Step::Tell(format!(
                    "Refused, nobody can approve it: {}",
                    permission.title
                )));
                steps
            }
            Event::MessageFinalized { message, .. }
                if Some(&message.actor) == self.agent.as_ref() =>
            {
                self.answer = Some(message.text.clone());
                Vec::new()
            }
            Event::Notice { text, .. } => vec![Step::Tell(text.clone())],
            _ => Vec::new(),
        }
    }

    /// A signal asks the launch to stop: cut every running turn. A message
    /// not sent yet is never sent.
    pub fn interrupt(&mut self) -> Vec<Step> {
        self.interrupted = true;
        self.message = None;
        let runs: Vec<RunId> = self.live.iter().cloned().collect();
        runs.into_iter().filter_map(|run| self.cut(run)).collect()
    }

    /// How the launch ended, once nothing more is coming for it.
    pub fn ending(&self) -> Option<Ending> {
        let sent = self.message.is_none() && self.requests > 0;
        let settled = self.refused
            || (self.finished.is_some() && self.live.is_empty())
            || (self.interrupted && !sent);
        if !settled {
            return None;
        }
        Some(if self.interrupted {
            Ending::Interrupted
        } else if self.refused || self.failed {
            Ending::Failed
        } else {
            Ending::Completed
        })
    }

    /// The agent's latest finished message.
    pub fn answer(&self) -> Option<&str> {
        self.answer.as_deref()
    }

    /// The conversation the message went to, and the agent that answers it.
    pub fn conversation(&self) -> Option<(&ConversationId, &ActorId)> {
        self.conversation.as_ref().zip(self.agent.as_ref())
    }

    /// Send the message to the agent the first snapshot selects.
    fn submit(&mut self, snapshot: &Snapshot) -> Vec<Step> {
        let Some(text) = self.message.take() else {
            return Vec::new();
        };
        self.conversation = Some(snapshot.conversation.clone());
        self.agent = Some(snapshot.selected_agent.clone());
        let submit = Command::Submit {
            agent: snapshot.selected_agent.clone(),
            text,
        };
        self.request_as(MESSAGE.into(), submit)
            .into_iter()
            .collect()
    }

    fn cut(&mut self, run: RunId) -> Option<Step> {
        self.request(Command::Interrupt { run })
    }

    /// A request under an operation of its own.
    fn request(&mut self, command: Command) -> Option<Step> {
        let operation = format!("headless-{}", self.requests + 1);
        self.request_as(operation.into(), command)
    }

    fn request_as(&mut self, operation: OperationId, command: Command) -> Option<Step> {
        let conversation = self.conversation.clone()?;
        self.requests += 1;
        let id = RequestId::new(self.requests);
        Some(Step::Send(Request::new(
            id,
            operation,
            conversation,
            command,
        )))
    }
}

#[cfg(test)]
mod tests;
