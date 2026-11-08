//! The model's requests to change mode: entering Plan, and leaving it for Ask
//! once the user approved a written plan. Leaving has its own card, which
//! shows the stored plan, never a text the model supplied; the gate then pins
//! the plan and revision shown, and the tool refuses one that changed since.
//! Entering uses the gate's ordinary card, and runs at once in Yolo like any
//! selected tool. Only the user chooses Yolo.

use std::sync::Arc;

use roomkit::ToolCallEvent;
use roomkit::model::{Metadata, ToolCall};
use roomkit_ai::{AiTool, Tool, ToolError, ToolFuture, ToolResult, current_tool_call};
use roomkit_tools_plans::{Plan, PlanScope, StepStatus};
use roomkit_tui::protocol::Decision;
use roomkit_tui_engine::{ConversationChannel, PermissionRequest};
use serde::Deserialize;
use serde_json::json;

use super::{Mode, Policy, Verdict, block, room_of};
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

    /// Both mode tools, each serving its own calls.
    pub fn tools(&self) -> [Arc<dyn Tool>; 2] {
        [ENTER, EXIT].map(|name| -> Arc<dyn Tool> {
            Arc::new(ModeTool {
                tools: self.clone(),
                name,
            })
        })
    }

    fn enter(&self, scope: &PlanScope, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let input: EnterInput = roomkit_ai::arguments(call)?;
        self.policy.apply(&scope.room_id, Mode::Plan);
        Ok(ToolResult::Json(json!({
            "mode": "plan",
            "reason": input.reason,
            "next": "The mode is now Plan; it replaces any earlier mode instruction in this turn. Explore and write the plan with the plan tools, without changing the project or running commands, then call plan_mode_exit for the user's approval."
        })))
    }

    async fn exit(&self, scope: &PlanScope, call: &ToolCall) -> Result<ToolResult, ToolError> {
        let input: ExitInput = roomkit_ai::arguments(call)?;
        let (Some(id), Some(approved)) = (input.plan_id, input.revision) else {
            return Err(ToolError::refused(
                "plan_mode_exit runs only once the user approved a plan",
            ));
        };
        let plan = self
            .plans
            .plan(scope, Some(&id))
            .await
            .map_err(|error| ToolError::refused(error.to_string()))?;
        if plan.revision != approved {
            return Err(ToolError::refused(
                "The plan changed while the user was approving it, so it was not approved and the mode is still Plan. Call plan_mode_exit again for the current plan.",
            ));
        }
        self.policy.apply(&scope.room_id, Mode::Ask);
        Ok(ToolResult::Json(json!({
            "mode": "ask",
            "approved_plan_id": plan.plan_id,
            "approved_revision": plan.revision,
            "next": "The user approved this plan and the mode is now Ask; it replaces the Plan instruction earlier in this turn. Implement the plan now. Each change and command still asks for approval."
        })))
    }
}

/// One mode tool: entering Plan, or leaving it.
struct ModeTool {
    tools: ModeTools,
    name: &'static str,
}

impl Tool for ModeTool {
    fn definition(&self) -> AiTool {
        if self.name == ENTER {
            AiTool {
                name: ENTER.into(),
                description: "Ask the user to move this conversation to Plan mode before substantial or ambiguous work: several files, several possible approaches, an architecture choice. Not for a small, clear change. The user approves or refuses (in Yolo it applies at once); in Plan you explore and write a plan with the plan tools, without changing the project or running commands.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "reason": {"type": "string", "maxLength": 1000, "description": "Why this task needs a plan first, shown to the user."}
                    },
                    "required": ["reason"],
                    "additionalProperties": false
                }),
                tags: Vec::new(),
                changes_state: false,
            }
        } else {
            AiTool {
                name: EXIT.into(),
                description: "In Plan mode, ask the user to approve a written plan and move this conversation to Ask mode to implement it. The user sees the stored plan, not your summary. Give plan_id; it may be omitted only when this conversation has a single plan. Approved, you implement it, each change still asking for approval; otherwise you stay in Plan and refine it.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "plan_id": {"type": "string", "description": "The plan to approve; required when this conversation has several plans."},
                        "revision": {"type": "integer", "minimum": 0, "description": "Set by the application to the revision the user approved; omit it."}
                    },
                    "additionalProperties": false
                }),
                tags: Vec::new(),
                changes_state: false,
            }
        }
    }

    fn handler(&self, name: String, arguments: Metadata) -> ToolFuture {
        if name != self.name {
            let unserved = ToolError::unserved(format!("'{}' does not serve '{name}'", self.name));
            return Box::pin(async move { Err(unserved) });
        }
        let call = current_tool_call();
        let scope = call.as_ref().and_then(|call| {
            Some(PlanScope {
                room_id: call.room_id.as_ref()?.to_string(),
                caller_id: call.channel_id.as_ref()?.to_string(),
            })
        });
        let id = call.map(|call| call.tool_call_id).unwrap_or_default();
        let call = ToolCall::new(id, name, arguments);
        let tools = self.tools.clone();
        Box::pin(async move {
            let scope = scope.ok_or_else(|| {
                ToolError::refused("mode tools require a conversation and an acting agent")
            })?;
            if call.name == ENTER {
                tools.enter(&scope, &call)
            } else {
                tools.exit(&scope, &call).await
            }
        })
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
) -> Verdict {
    let room = room_of(&event);
    if policy.mode(room) != Mode::Plan {
        return block("plan_mode_exit applies in Plan mode only; the mode is unchanged.");
    }
    if policy.delegated(room) {
        return block(super::DELEGATED);
    }
    let plan = match shown_plan(plans, &event).await {
        Ok(plan) => plan,
        Err(refusal) => return block(refusal),
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
        Decision::Allow if policy.mode(room) == Mode::Plan => pinned(event, &plan),
        Decision::Allow => {
            block("The mode changed while approval was pending; the plan was not approved.")
        }
        _ => block(
            "The plan was not approved (refused, expired or view disconnected): stay in Plan, refine the plan if the user asks, and ask again.",
        ),
    }
}

/// The plan the card shows: the one named, or the conversation's only one.
async fn shown_plan(plans: &Plans, event: &ToolCallEvent) -> Result<Plan, String> {
    let id = event
        .arguments
        .get("plan_id")
        .and_then(serde_json::Value::as_str);
    let scope = PlanScope {
        room_id: room_of(event).to_owned(),
        caller_id: event.channel_id.to_string(),
    };
    plans
        .plan(&scope, id)
        .await
        .map_err(|error| error.to_string())
}

/// The call, its arguments pinned to the plan and revision the user approved.
fn pinned(mut event: ToolCallEvent, plan: &Plan) -> Verdict {
    let mut arguments = Metadata::new();
    arguments.insert("plan_id".into(), json!(plan.plan_id));
    arguments.insert("revision".into(), json!(plan.revision));
    event.arguments = arguments;
    roomkit::model::HookResult::modify(event, Default::default())
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
