//! The model's requests to change mode: entering Plan, and leaving it for Ask
//! once the user approved a written plan. The user approves each, through the
//! permission gate: entering with the gate's ordinary card, leaving with a
//! card that shows the stored plan, never a text the model supplied. Only the
//! user chooses Yolo.

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
}

impl ModeTools {
    pub fn new(policy: Policy, plans: Arc<Plans>) -> Self {
        Self { policy, plans }
    }
}

impl Toolset for ModeTools {
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition::new(
                ENTER,
                "Ask the user to move this conversation to Plan mode before substantial or ambiguous work: several files, several possible approaches, an architecture choice. Not for a small, clear change. The user approves or refuses; in Plan you explore and write a plan with the plan tools, without changing the project or running commands.",
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
                "In Plan mode, ask the user to approve a written plan and move this conversation to Ask mode to implement it. The user sees the stored plan, not your summary; omit plan_id for the plan this conversation follows. Approved, you implement it, each change still asking for approval; refused, you stay in Plan and refine it.",
                json!({
                    "type": "object",
                    "properties": {
                        "plan_id": {"type": "string", "description": "The plan to approve; the plan this conversation follows when omitted."}
                    },
                    "additionalProperties": false
                }),
            ),
        ]
    }

    async fn call(&self, call: ToolCall, context: ToolContext) -> Result<ToolOutput, ToolError> {
        let room = context.room_id.as_str();
        match call.name.as_str() {
            ENTER => {
                let input: EnterInput = roomkit_ai::arguments(&call)?;
                self.policy.set_by_tool(room, Mode::Plan);
                Ok(ToolOutput::Json(json!({
                    "mode": "plan",
                    "reason": input.reason,
                    "next": "Explore and write the plan with the plan tools, then call plan_mode_exit for the user's approval."
                })))
            }
            EXIT => {
                let input: ExitInput = roomkit_ai::arguments(&call)?;
                let scope = scope(&context);
                let plan = self
                    .plans
                    .plan(&scope, input.plan_id.as_deref())
                    .await
                    .map_err(|error| ToolError::refused(error.to_string()))?;
                self.policy.set_by_tool(room, Mode::Ask);
                Ok(ToolOutput::Json(json!({
                    "mode": "ask",
                    "approved_plan_id": plan.plan_id,
                    "next": "The user approved this plan: implement it now. Each change and command still asks for approval."
                })))
            }
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

/// The approval of `plan_mode_exit`: only in Plan, for a stored plan, shown
/// whole on the card, with a hook's reason first when one asked for review.
pub(crate) async fn approve_exit(
    policy: &Policy,
    channel: &ConversationChannel,
    plans: &Plans,
    event: ToolCallEvent,
    review: Option<String>,
) -> ToolUse {
    if policy.mode(event.room_id.as_str()) != Mode::Plan {
        return ToolUse::block("plan_mode_exit applies in Plan mode only; the mode is unchanged.");
    }
    let id = event
        .call
        .arguments
        .get("plan_id")
        .and_then(serde_json::Value::as_str);
    let scope = PlanScope {
        room_id: event.room_id.to_string(),
        caller_id: event.channel_id.to_string(),
    };
    let plan = match plans.plan(&scope, id).await {
        Ok(plan) => plan,
        Err(error) => return ToolUse::block(error.to_string()),
    };
    let mut detail = render(&plan);
    if let Some(reason) = review {
        detail = format!("Hook requires review: {reason}\n\n{detail}");
    }
    let request = PermissionRequest::new(
        "Approve this plan and implement it in Ask mode?",
        detail,
        crate::timeouts::APPROVAL,
    )
    .with_summary(format!("{EXIT} · {}", plan.title));
    match channel.ask_tool_permission(&event, request).await {
        Decision::Allow if policy.mode(event.room_id.as_str()) == Mode::Plan => ToolUse::Allow,
        Decision::Allow => ToolUse::block(
            "The mode changed while approval was pending; the plan was not approved.",
        ),
        _ => ToolUse::block(
            "The user did not approve this plan: stay in Plan mode, ask what to change, refine the plan and ask again.",
        ),
    }
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
