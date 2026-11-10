//! What an unattended launch says to the host, and when it is done, decided
//! from the host's events alone: the transport, the signals and the output
//! stay with the caller.

use std::collections::BTreeSet;

use roomkit_tui::protocol::{
    ActorId, Command, ConversationId, Decision, Event, OperationId, Permission, Rejection, Request,
    RequestId, RunId, RunOutcome, Snapshot,
};

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
    /// The message was refused, or the turn ended without its answer: on its
    /// round limit, an error, or a cut nobody asked for.
    Failed,
    /// A signal stopped the launch before its turn ended.
    Interrupted,
}

/// The conversation as the launch knows it.
pub(super) struct Driver {
    /// This launch's identity, which names every operation it asks for: the
    /// host takes an operation it already holds, from an earlier launch on the
    /// same conversation, for a replay and drops it.
    launch: String,
    /// The message, until the first snapshot names where to send it.
    message: Option<String>,
    /// The request that sent the message, once sent.
    sent: Option<RequestId>,
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
    /// A run or the message's operation ended cut.
    cut: bool,
    interrupted: bool,
    /// The agent's latest finished message.
    answer: Option<String>,
}

impl Driver {
    pub fn new(message: String, launch: String) -> Self {
        Self {
            launch,
            message: Some(message),
            sent: None,
            conversation: None,
            agent: None,
            requests: 0,
            refused: false,
            finished: None,
            live: BTreeSet::new(),
            failed: false,
            cut: false,
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
            } => self.declined(*request, rejection),
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
                self.ended(*outcome);
                Vec::new()
            }
            Event::OperationFinished {
                operation, outcome, ..
            } if *operation == self.message_operation() => {
                self.finished = Some(*outcome);
                self.ended(*outcome);
                Vec::new()
            }
            Event::PermissionRequested { permission, .. } => self.refuse(permission),
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

    /// How the launch ended, once nothing more is coming for it. A signal
    /// that came once the turn was over changes nothing.
    pub fn ending(&self) -> Option<Ending> {
        let sent = self.sent.is_some();
        let settled = self.refused
            || (self.finished.is_some() && self.live.is_empty())
            || (self.interrupted && !sent);
        if !settled {
            return None;
        }
        Some(if self.interrupted && (self.cut || !sent) {
            Ending::Interrupted
        } else if self.refused || self.failed || self.answer.is_none() {
            Ending::Failed
        } else {
            Ending::Completed
        })
    }

    /// The agent's latest finished message, without the blank lines a model
    /// may open or close it on.
    pub fn answer(&self) -> Option<&str> {
        self.answer.as_deref().map(without_blank_edges)
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
        let step = self.request_as(self.message_operation(), submit);
        self.sent = Some(RequestId::new(self.requests));
        step.into_iter().collect()
    }

    /// What a turn's or the operation's outcome says of the launch.
    fn ended(&mut self, outcome: RunOutcome) {
        self.failed |= outcome != RunOutcome::Completed;
        self.cut |= outcome == RunOutcome::Interrupted;
    }

    /// The host declined a request: the message, or a decision or a cut.
    fn declined(&mut self, request: RequestId, rejection: &Rejection) -> Vec<Step> {
        let text = if Some(request) == self.sent {
            self.refused = true;
            format!("The message was refused: {}", rejection.message)
        } else {
            format!(
                "A request to the conversation was declined: {}",
                rejection.message
            )
        };
        vec![Step::Tell(text)]
    }

    /// Nobody can approve: refuse, and say so.
    fn refuse(&mut self, permission: &Permission) -> Vec<Step> {
        let decide = Command::Decide {
            permission: permission.id.clone(),
            decision: Decision::Deny,
        };
        let mut steps: Vec<Step> = self.request(decide).into_iter().collect();
        let title = &permission.title;
        steps.push(Step::Tell(format!(
            "Refused, nobody can approve it: {title}"
        )));
        steps
    }

    fn cut(&mut self, run: RunId) -> Option<Step> {
        self.request(Command::Interrupt { run })
    }

    fn message_operation(&self) -> OperationId {
        format!("{}-message", self.launch).into()
    }

    /// A request under an operation of its own.
    fn request(&mut self, command: Command) -> Option<Step> {
        let operation = format!("{}-{}", self.launch, self.requests + 1);
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

/// `text` from its first line holding something, indentation kept, to the end
/// of its last.
fn without_blank_edges(text: &str) -> &str {
    let blank = text.len() - text.trim_start().len();
    let first_line = text[..blank].rfind('\n').map_or(0, |newline| newline + 1);
    text[first_line..].trim_end()
}

#[cfg(test)]
mod tests;
