//! The tool through which an agent asks the person questions, `ask_user`,
//! served by RoomKit's human input: the kit opens a request and waits for its
//! answer; an `ON_USER_INPUT_REQUIRED` hook shows the request's questions to
//! the conversation's view, whose answer, or refusal, is what the agent reads.
//! Only an attended launch offers the tool, and only a conversation's own
//! agent may ask: a delegated agent puts its question in its task's result.

use std::time::Duration;

use roomkit::human_input::PendingInputEvent;
use roomkit::model::hook::AllowOptions;
use roomkit::model::{ChannelId, HookResult, RoomId};
use roomkit::{HookOptions, HookTrigger, RejectInputOptions, RoomKit};
use roomkit_ai::{AiTool, HumanInputToolHandler, HumanInputToolHandlerOptions};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::{QuestionItem, QuestionOption};
use roomkit_tui_engine::{ConversationChannel, QuestionRequest};
use serde_json::{Map, Value, json};

use crate::Result;
use crate::permissions::Policy;

/// The tool's name.
pub(crate) const ASK_USER: &str = "ask_user";

/// How long a question waits for its answer. A turn waits on it as on an
/// approval, and the person may be away for a while.
const WAIT: Duration = Duration::from_secs(24 * 60 * 60);

const MAX_QUESTIONS: usize = 4;
const MAX_OPTIONS: usize = 4;
const MAX_HEADER_CHARS: usize = 12;

const DESCRIPTION: &str = "Ask the user one to four questions and wait for the answers. \
Use it only when you are blocked on a decision that is genuinely theirs: a preference, a choice \
between valid approaches, information the project cannot tell you. Do not use it to ask for \
permission to act (approvals handle that), to ask whether to continue, or for what you can find by \
reading the project. Ask independent questions in one call. Offer two to four options when the \
answer is a choice, the most likely first, its label ending with \"(Recommended)\" when you \
recommend it; leave the options empty for a free answer. The user can always answer in their own \
words: never add an \"Other\" option. Never write a multiple-choice question as a message instead. \
If the user does not answer, continue with your best judgement and do not ask again.";

/// The tool as the model is shown it.
pub(crate) fn definition() -> AiTool {
    let option = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["label"],
        "properties": {
            "label": {"type": "string", "description": "One to five words: what the user picks."},
            "description": {"type": "string", "description": "What picking it means."}
        }
    });
    let question = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["question"],
        "properties": {
            "header": {"type": "string", "description": "A label of at most 12 characters naming the question."},
            "question": {"type": "string", "description": "The question, ending with a question mark."},
            "options": {"type": "array", "maxItems": MAX_OPTIONS, "items": option},
            "multi_select": {"type": "boolean", "description": "Whether the user may pick several options."}
        }
    });
    AiTool {
        name: ASK_USER.to_owned(),
        description: DESCRIPTION.to_owned(),
        parameters: json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["questions"],
            "properties": {
                "questions": {"type": "array", "minItems": 1, "maxItems": MAX_QUESTIONS, "items": question}
            }
        }),
        tags: Vec::new(),
        changes_state: false,
        defer_loading: false,
    }
}

/// What serves the tool: the kit's human input, waiting [`WAIT`] at most.
pub(crate) fn handler() -> Result<HumanInputToolHandler> {
    let options = HumanInputToolHandlerOptions {
        timeout: WAIT,
        tool_definitions: vec![definition()],
        ..HumanInputToolHandlerOptions::default()
    };
    Ok(HumanInputToolHandler::new([ASK_USER], options)?)
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
    let Some(asked) = arguments.get("questions").and_then(Value::as_array) else {
        return Err("ask_user takes `questions`, a list of one to four questions.".into());
    };
    if asked.is_empty() || asked.len() > MAX_QUESTIONS {
        return Err(format!(
            "ask_user takes one to {MAX_QUESTIONS} questions; {} were given.",
            asked.len()
        ));
    }
    asked
        .iter()
        .enumerate()
        .map(|(index, question)| item(index, question))
        .collect()
}

fn item(index: usize, question: &Value) -> std::result::Result<QuestionItem, String> {
    let text = |name: &str| {
        question
            .get(name)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
    };
    let number = index + 1;
    let asked = text("question");
    if asked.is_empty() {
        return Err(format!("Question {number} has no text."));
    }
    let header: String = text("header").chars().take(MAX_HEADER_CHARS).collect();
    let options = question
        .get("options")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if options.len() > MAX_OPTIONS {
        return Err(format!(
            "Question {number} offers {} options; at most {MAX_OPTIONS}.",
            options.len()
        ));
    }
    let mut labels = std::collections::BTreeSet::new();
    let mut offered = Vec::new();
    for option in &options {
        let label = option
            .get("label")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if label.is_empty() || !labels.insert(label.to_owned()) {
            return Err(format!(
                "Question {number} needs distinct, non-empty option labels."
            ));
        }
        let description = option
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        offered.push(QuestionOption::new(label, description.trim()));
    }
    let multi = question
        .get("multi_select")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(QuestionItem::new(header, asked, offered).with_multi_select(multi))
}

#[cfg(test)]
mod tests;
