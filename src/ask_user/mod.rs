//! The tool through which an agent asks the person questions, `ask_user`,
//! from roomkit-tools, served by RoomKit's human input: the kit opens a
//! request and waits for its answer; an `ON_USER_INPUT_REQUIRED` hook shows
//! the request's questions to the conversation's view, whose answer, or
//! refusal, is what the agent reads.
//! Only an attended launch offers the tool, and only a conversation's own
//! agent may ask: a delegated agent puts its question in its task's result.

use std::time::Duration;

use roomkit::human_input::PendingInputEvent;
use roomkit::model::hook::AllowOptions;
use roomkit::model::{ChannelId, HookResult, RoomId};
use roomkit::{HookOptions, HookTrigger, RejectInputOptions, RoomKit};
use roomkit_ai::HumanInputToolHandler;
use roomkit_sqlite::SqliteStore;
use roomkit_tools_agents::{AskUserQuestion, AskUserTool};
use roomkit_tools_core::ToolError;
use roomkit_tui::protocol::{QuestionItem, QuestionOption};
use roomkit_tui_engine::{ConversationChannel, QuestionRequest};
use serde_json::{Map, Value};

use crate::Result;
use crate::permissions::Policy;

/// The tool's name.
pub(crate) const ASK_USER: &str = "ask_user";

/// How long a question waits for its answer. A turn waits on it as on an
/// approval, and the person may be away for a while.
const WAIT: Duration = Duration::from_secs(24 * 60 * 60);

/// The width of a question's header in the view, which the tool asks of the
/// model.
const MAX_HEADER_CHARS: usize = 12;

/// What serves the tool: the kit's human input, waiting [`WAIT`] at most.
pub(crate) fn handler() -> Result<HumanInputToolHandler> {
    Ok(roomkit_tools_roomkit::ask_user_handler(WAIT)?)
}

/// Show the conversation's view the questions of each `ask_user` request,
/// or reject the request with what the agent should do instead: a hook's own
/// refusal would reach the agent without its reason.
pub(crate) fn install(
    kit: &RoomKit<SqliteStore>,
    channel: ConversationChannel,
    policy: Policy,
) -> Result<()> {
    let engine = kit.downgrade();
    kit.hook(
        HookTrigger::OnUserInputRequired,
        move |event: PendingInputEvent, _context| {
            let (channel, policy, engine) = (channel.clone(), policy.clone(), engine.clone());
            async move {
                if let Err(reason) = show(&channel, &policy, &event)
                    && let Some(kit) = engine.upgrade()
                {
                    let options = RejectInputOptions { reason };
                    let _ = kit.reject_input(&event.pending_id, options).await;
                }
                HookResult::allow(AllowOptions::default())
            }
        },
        HookOptions {
            name: "ask-user".into(),
            ..HookOptions::default()
        },
    )?;
    Ok(())
}

/// Shows the questions of `event` to the view, or says why the agent cannot
/// ask them and what it should do instead. Requests of other tools are not
/// its own.
fn show(
    channel: &ConversationChannel,
    policy: &Policy,
    event: &PendingInputEvent,
) -> std::result::Result<(), String> {
    if event.tool_name != ASK_USER {
        return Ok(());
    }
    if policy.delegated(&event.room_id) {
        return Err(
            "Only the conversation's agent can ask the user. Put your question, with the \
                    options you see, in your task's result."
                .into(),
        );
    }
    let items = questions(&event.arguments)?;
    let (Ok(room), Ok(agent)) = (
        RoomId::new(&event.room_id),
        ChannelId::new(&event.channel_id),
    ) else {
        return Err("This question has no conversation to be asked in.".into());
    };
    let request = QuestionRequest::new(event.pending_id.clone(), items)
        .with_tool_call(event.tool_call_id.clone());
    if channel.ask_question(&room, &agent, request) {
        Ok(())
    } else {
        Err(
            "No one can answer here. Continue with your best judgement and say which \
             assumptions you make."
                .into(),
        )
    }
}

/// The questions a call carries, or what the model should fix in them.
pub(crate) fn questions(
    arguments: &Map<String, Value>,
) -> std::result::Result<Vec<QuestionItem>, String> {
    let input = AskUserTool::read(arguments).map_err(|error| match error {
        ToolError::InvalidInput(fix) | ToolError::LimitExceeded(fix) => fix,
        other => other.to_string(),
    })?;
    Ok(input.questions.iter().map(item).collect())
}

/// A question as the view shows it: trimmed, its header cut to the view's
/// width.
fn item(question: &AskUserQuestion) -> QuestionItem {
    let header: String = question
        .header
        .trim()
        .chars()
        .take(MAX_HEADER_CHARS)
        .collect();
    let options = question
        .options
        .iter()
        .map(|option| QuestionOption::new(option.label.trim(), option.description.trim()))
        .collect();
    QuestionItem::new(header, question.question.trim(), options)
        .with_multi_select(question.multi_select)
}

#[cfg(test)]
mod tests;
