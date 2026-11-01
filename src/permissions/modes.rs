//! The model's requests to change mode: entering Plan, and leaving it for Ask
//! once the user approved a written plan. Leaving has its own card, which
//! shows the stored plan, never a text the model supplied; the gate then pins
//! the plan and revision shown, and the tool refuses one that changed since.
//! Entering uses the gate's ordinary card, and runs at once in Yolo like any
//! selected tool. Only the user chooses Yolo.

use std::sync::Arc;

use roomkit::model::ToolCall;
use roomkit::{ToolCallEvent, ToolUse};
use roomkit_ai::{ToolContext, ToolDefinition, ToolError, ToolOutput, Toolset};
use roomkit_tools_plans::{Plan, PlanScope, StepStatus};
use roomkit_tui::protocol::Decision;
use roomkit_tui_engine::{ConversationChannel, PermissionRequest};
use serde::Deserialize;
use serde_json::json;

use super::{Mode, Policy};
use crate::plans::Plans;

pub(crate) const ENTER: &str = "plan_mode_enter";
pub(crate) const EXIT: &str = "plan_mode_exit";

/// The mode tools, available to an agent that can write plans.
#[derive(Clone)]
pub(crate) struct ModeTools {
    policy: Policy,
    plans: Arc<Plans>,
}

impl std::fmt::Debug for ModeTools {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ModeTools")
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterInput {
    reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExitInput {
    #[serde(default)]
    plan_id: Option<String>,
    /// Set by the gate to the revision the user approved.
    #[serde(default)]
    revision: Option<u64>,
}

impl ModeTools {
    pub fn new(policy: Policy, plans: Arc<Plans>) -> Self {
        Self { policy, plans }
    }

    fn enter(&self, room: &str, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let input: EnterInput = roomkit_ai::arguments(call)?;
        self.policy.apply(room, Mode::Plan);
        Ok(ToolOutput::Json(json!({
            "mode": "plan",
            "reason": input.reason,
            "next": "The mode is now Plan; it replaces any earlier mode instruction in this turn. Explore and write the plan with the plan tools, without changing the project or running commands, then call plan_mode_exit for the user's approval."
        })))
    }

    async fn exit(&self, context: &ToolContext, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let input: ExitInput = roomkit_ai::arguments(call)?;
        let (Some(id), Some(approved)) = (input.plan_id, input.revision) else {
            return Err(ToolError::refused(
                "plan_mode_exit runs only once the user approved a plan",
            ));
        };
        let plan = self
            .plans
            .plan(&scope(context), Some(&id))
            .await
            .map_err(|error| ToolError::refused(error.to_string()))?;
        if plan.revision != approved {
            return Err(ToolError::refused(
                "The plan changed while the user was approving it, so it was not approved and the mode is still Plan. Call plan_mode_exit again for the current plan.",
            ));
        }
        self.policy.apply(context.room_id.as_str(), Mode::Ask);
        Ok(ToolOutput::Json(json!({
            "mode": "ask",
            "approved_plan_id": plan.plan_id,
            "approved_revision": plan.revision,
            "next": "The user approved this plan and the mode is now Ask; it replaces the Plan instruction earlier in this turn. Implement the plan now. Each change and command still asks for approval."
        })))
    }
}

impl Toolset for ModeTools {
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                ENTER,
                "Ask the user to move this conversation to Plan mode before substantial or ambiguous work: several files, several possible approaches, an architecture choice. Not for a small, clear change. The user approves or refuses (in Yolo it applies at once); in Plan you explore and write a plan with the plan tools, without changing the project or running commands.",
                json!({
                    "type": "object",
                    "properties": {
                        "reason": {"type": "string", "maxLength": 1000, "description": "Why this task needs a plan first, shown to the user."}
                    },
                    "required": ["reason"],
                    "additionalProperties": false
                }),
            ),
            ToolDefinition::new(
                EXIT,
                "In Plan mode, ask the user to approve a written plan and move this conversation to Ask mode to implement it. The user sees the stored plan, not your summary. Give plan_id; it may be omitted only when this conversation has a single plan. Approved, you implement it, each change still asking for approval; otherwise you stay in Plan and refine it.",
                json!({
                    "type": "object",
                    "properties": {
                        "plan_id": {"type": "string", "description": "The plan to approve; required when this conversation has several plans."},
                        "revision": {"type": "integer", "minimum": 0, "description": "Set by the application to the revision the user approved; omit it."}
                    },
                    "additionalProperties": false
                }),
            ),
        ]
    }

    async fn call(&self, call: ToolCall, context: ToolContext) -> Result<ToolOutput, ToolError> {
        match call.name.as_str() {
            ENTER => self.enter(context.room_id.as_str(), &call),
            EXIT => self.exit(&context, &call).await,
            _ => Err(ToolError::refused("unavailable mode tool")),
        }
    }
}

fn scope(context: &ToolContext) -> PlanScope {
    PlanScope {
        room_id: context.room_id.to_string(),
        caller_id: context.channel_id.to_string(),
    }
}

/// The approval of `plan_mode_exit`: only in Plan, for a stored plan shown
/// whole, a hook's reason first when one asked for review. Approved, the
/// call goes on pinned to the plan and revision shown.
pub(crate) async fn approve_exit(
    policy: &Policy,
    channel: &ConversationChannel,
    plans: &Plans,
    event: ToolCallEvent,
    review: Option<String>,
) -> ToolUse {
    let room = event.room_id.as_str();
    if policy.mode(room) != Mode::Plan {
        return ToolUse::block("plan_mode_exit applies in Plan mode only; the mode is unchanged.");
    }
    let plan = match shown_plan(plans, &event).await {
        Ok(plan) => plan,
        Err(refusal) => return refusal,
    };
    let detail = match review {
        Some(reason) => format!("Hook requires review: {reason}\n\n{}", render(&plan)),
        None => render(&plan),
    };
    let request = PermissionRequest::new(
        "Approve this plan and implement it in Ask mode?",
        detail,
        crate::timeouts::APPROVAL,
    )
    .with_summary(format!("{EXIT} · {}", plan.title));
    match channel.ask_tool_permission(&event, request).await {
        Decision::Allow if policy.mode(room) == Mode::Plan => pinned(&plan),
        Decision::Allow => ToolUse::block(
            "The mode changed while approval was pending; the plan was not approved.",
        ),
        _ => ToolUse::block(
            "The plan was not approved (refused, expired or view disconnected): stay in Plan, refine the plan if the user asks, and ask again.",
        ),
    }
}

/// The plan the card shows: the one named, or the conversation's only one.
async fn shown_plan(plans: &Plans, event: &ToolCallEvent) -> Result<Plan, ToolUse> {
    let id = event
        .call
        .arguments
        .get("plan_id")
        .and_then(serde_json::Value::as_str);
    let scope = PlanScope {
        room_id: event.room_id.to_string(),
        caller_id: event.channel_id.to_string(),
    };
    plans
        .plan(&scope, id)
        .await
        .map_err(|error| ToolUse::block(error.to_string()))
}

/// The call's arguments, pinned to the plan and revision the user approved.
fn pinned(plan: &Plan) -> ToolUse {
    let mut arguments = roomkit::model::Metadata::new();
    arguments.insert("plan_id".into(), json!(plan.plan_id));
    arguments.insert("revision".into(), json!(plan.revision));
    ToolUse::Rewrite(arguments)
}

/// A plan as the user approves it: title, objective, then numbered steps.
fn render(plan: &Plan) -> String {
    let mut text = format!("Plan · {}\n\n{}\n", plan.title, plan.objective);
    for (index, step) in plan.steps.iter().enumerate() {
        let status = match step.status {
            StepStatus::Pending => "",
            StepStatus::InProgress => " (in progress)",
            StepStatus::Completed => " (completed)",
            StepStatus::Blocked => " (blocked)",
            StepStatus::Cancelled => " (cancelled)",
        };
        text.push_str(&format!("\n{}. {}{status}", index + 1, step.description));
    }
    text
}
