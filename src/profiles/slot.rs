//! Stable engine identity, with an immutable configuration pinned for each turn.
use roomkit::model::{
    ChannelBinding, ChannelCapabilities, ChannelCategory, ChannelId, ChannelMediaType,
    ChannelOutput, ChannelType, EventType, InboundMessage, RoomContext, RoomEvent,
    SteeringDirective, TextContent,
};
use roomkit::{Channel, ChannelDeclaration, ChannelError};
use std::sync::{Arc, RwLock};

use crate::agent::CodeAgent;

#[derive(Clone)]
pub(super) struct AgentSlot {
    instructions: Arc<crate::instructions::Instructions>,
    files: Arc<crate::files::Files>,
    compactions: Arc<crate::compaction::Compactions>,
    policy: crate::permissions::Policy,
    /// The agents a turn can hand a task to.
    roster: Arc<crate::delegation::Roster>,
    id: ChannelId,
    current: Arc<RwLock<Option<Arc<CodeAgent>>>>,
}
impl AgentSlot {
    pub fn new(
        id: ChannelId,
        agent: Option<CodeAgent>,
        policy: crate::permissions::Policy,
        instructions: Arc<crate::instructions::Instructions>,
        files: Arc<crate::files::Files>,
        compactions: Arc<crate::compaction::Compactions>,
        roster: Arc<crate::delegation::Roster>,
    ) -> Self {
        Self {
            instructions,
            files,
            compactions,
            policy,
            roster,
            id,
            current: Arc::new(RwLock::new(agent.map(Arc::new))),
        }
    }
    /// Whether the agent answers `event`: one addressed to it, as the view
    /// addresses a person's message to the selected agent, or in the room of
    /// a task handed to it, where the engine writes the task to the room's
    /// agents without addressing it (RMK-1532).
    fn answers(&self, event: &RoomEvent, context: &RoomContext) -> bool {
        match &event.addressed_to {
            Some(ids) => ids.contains(&self.id),
            None => context
                .room
                .metadata
                .get("task_agent_id")
                .and_then(|agent| agent.as_str())
                .is_some_and(|agent| agent == self.id.as_str()),
        }
    }
    pub fn replace(&self, agent: CodeAgent) {
        *crate::sync::write(&self.current) = Some(Arc::new(agent));
    }
    pub fn disable(&self) {
        *crate::sync::write(&self.current) = None;
    }
    /// What a turn reads as it starts: its mode's instruction, what a
    /// delegated task's worker must know, the agents it can hand a task to,
    /// then what the application tells it (`told`), then the project's
    /// instructions unless the agent's history still holds them as they are.
    async fn instruction(
        &self,
        event: &RoomEvent,
        history: &[RoomEvent],
        told: Option<String>,
    ) -> String {
        let (room, agent) = (event.room_id.as_str(), self.id.as_str());
        self.instructions.begin(room, agent);
        let project = self.instructions.load(vec![], true).await.render();
        self.compactions.tell_snapshot(room, agent, &project);
        let mut parts = vec![self.policy.instruction(room).to_owned()];
        if self.policy.delegated(room) {
            parts.push(crate::delegation::WORKER.to_owned());
        }
        parts.extend(self.roster.instruction(agent));
        parts.extend(told);
        if !self.still_read(&project, event, history) {
            parts.push(project);
        }
        parts.join("\n\n")
    }

    /// Whether the agent's turn on `event`, reading `history`, still holds
    /// `project` as the latest snapshot it read. Only a history that compacts
    /// by tiers is trusted to: a window of messages may overflow the model's,
    /// and the kit's emergency compaction keeps whole only the turn's input,
    /// where the snapshot then rides. A standalone instruction reads no
    /// history.
    fn still_read(&self, project: &str, event: &RoomEvent, history: &[RoomEvent]) -> bool {
        let (room, agent) = (event.room_id.as_str(), self.id.as_str());
        if roomkit_core::rules::instructions::is_standalone(event)
            || !self.compactions.bounded(agent)
        {
            return false;
        }
        let compacted = self.compactions.get(room, agent);
        let view = crate::instructions::View {
            from: compacted.as_ref().map_or(0, |compacted| compacted.from),
            carried: compacted
                .as_ref()
                .and_then(|compacted| compacted.snapshot.as_deref()),
        };
        crate::instructions::still_read(project, history, &self.id, event, &view)
    }

    /// What the agent receives for `event`. Keep the delivered user event intact,
    /// including images and its durable identity: the delegated channel receives
    /// an application Instruction after it, in this same turn. It is never
    /// committed as a user's words or inserted into the system prompt/tool
    /// schemas: the kit keeps it as the agent's own row, which its later turns
    /// replay where it was read (roomkit-rs RMK-1407). The engine's transcript
    /// renderer distinguishes application instructions from participant text.
    async fn turn_input(&self, event: RoomEvent, context: &mut RoomContext) -> RoomEvent {
        match event.r#type {
            EventType::Message => {
                if !context.recent_events.iter().any(|past| past.id == event.id) {
                    context.recent_events.push(event.clone());
                }
                let instruction = self.instruction(&event, &context.recent_events, None).await;
                let mut input = RoomEvent::new(
                    event.room_id.clone(),
                    event.source.clone(),
                    TextContent::new(instruction),
                );
                input.r#type = EventType::Instruction;
                input.addressed_to = event.addressed_to.clone();
                input.index = event.index;
                input
            }
            // A turn started without a message, as to answer one handed to a
            // turn that was cut: an instruction, which it reads with its mode
            // and the project's instructions, as any turn does.
            EventType::Instruction => {
                let told = roomkit_core::rules::transcoding::text_of(&event.content);
                let instruction = self
                    .instruction(&event, &context.recent_events, Some(told))
                    .await;
                let mut input = event;
                input.content = TextContent::new(instruction).into();
                input
            }
            _ => event,
        }
    }
}
impl Channel for AgentSlot {
    fn channel_id(&self) -> ChannelId {
        self.id.clone()
    }
    fn channel_type(&self) -> ChannelType {
        ChannelType::AI
    }
    fn category(&self) -> ChannelCategory {
        ChannelCategory::Intelligence
    }
    /// Stable transport accepts media; each turn selects an opt-in vision provider.
    fn capabilities(&self) -> ChannelCapabilities {
        let mut capabilities = ChannelCapabilities::default();
        capabilities.media_types.insert(ChannelMediaType::Media);
        capabilities.supports_media = true;
        capabilities
    }
    /// The history each agent's turn reads, a memory that compacts reading
    /// all of it since its last compaction: the kit sizes the context it
    /// hands the slot by it, as it reads the slot's declaration once, when
    /// the slot is registered, whatever agent it holds then.
    fn recent_events_window(&self) -> usize {
        crate::compaction::WINDOW
    }
    /// A message written while an agent's turn runs goes to that turn, which
    /// reads it at its next step. The kit reads this on the registered
    /// channel, the slot, whatever agent it holds.
    fn steers_room_turns(&self) -> bool {
        true
    }
    /// Steered turns run one after the other, as an AI channel that steers
    /// declares.
    fn serializes_room_turns(&self) -> bool {
        true
    }
    /// The kit hands a steered message to the turn through the registered
    /// channel: the agent holding the turn's loop reads it. An agent
    /// replaced since the turn began has no loop for it; the message then
    /// gets a turn of its own.
    fn steer_turn(&self, directive: &SteeringDirective, turn_id: &str) -> Option<usize> {
        let agent = crate::sync::read(&self.current).clone()?;
        agent.steer_turn(directive, turn_id)
    }
    /// The slot's own answers, and the tools a room serves its turns, as an AI
    /// channel takes them: `delegate_task`, `task_status` and `cancel_task`
    /// (RMK-1532). The kit reads this on the registered slot, once.
    fn declaration(&self) -> ChannelDeclaration {
        let mut declared = ChannelDeclaration::new(self.channel_id());
        declared.channel_type = self.channel_type();
        declared.category = self.category();
        declared.capabilities = self.capabilities();
        declared.takes_owner_tools = true;
        declared.recent_events_window = self.recent_events_window();
        declared.serializes_room_turns = self.serializes_room_turns();
        declared.steers_room_turns = self.steers_room_turns();
        declared
    }
    async fn handle_inbound(
        &self,
        _: InboundMessage,
        _: RoomContext,
    ) -> Result<RoomEvent, ChannelError> {
        Err("AI channel does not accept inbound messages".into())
    }
    /// An intelligence channel is not delivered to: its turn is its
    /// `on_event()`.
    async fn deliver(
        &self,
        _: RoomEvent,
        _: ChannelBinding,
        _: RoomContext,
    ) -> Result<ChannelOutput, ChannelError> {
        Ok(ChannelOutput::empty())
    }
    async fn on_event(
        &self,
        event: RoomEvent,
        binding: ChannelBinding,
        mut context: RoomContext,
    ) -> Result<ChannelOutput, ChannelError> {
        if !self.answers(&event, &context) {
            return Ok(ChannelOutput::default());
        }
        let agent = crate::sync::read(&self.current)
            .clone()
            .ok_or("Configure a provider and agent first")?;
        // A delegated task's room takes its conversation's mode.
        self.policy.adopt(&context.room);
        let input = self.turn_input(event, &mut context).await;
        agent.on_event(input, binding, context).await
    }
    /// Records what the shell calls the kit's close cut changed, while the
    /// kit's store is still open: it closes the store after its channels.
    /// The agents share one file tracker, which waits for them once. Then
    /// closes the current agent's provider; the host closes what serves its
    /// tools.
    async fn close(&self) {
        self.files.settle(crate::timeouts::SETTLE).await;
        let agent = crate::sync::read(&self.current).clone();
        if let Some(agent) = agent {
            agent.close().await;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "A broken fixture fails the test")]
    use super::*;

    /// The kit sizes the history it hands a turn by the largest window the
    /// room's channels declare, read once as the slot is registered: the
    /// slot declares its agents' history, whatever agent it holds then. It
    /// also reads there that the agents steer their turns.
    #[test]
    fn the_slot_declares_its_agents_history_media_and_steering() {
        let root = tempfile::tempdir().unwrap();
        let slot = AgentSlot::new(
            ChannelId::new("code").unwrap(),
            None,
            crate::permissions::Policy::new(crate::permissions::Mode::Ask),
            crate::instructions::Instructions::new(root.path(), root.path()),
            crate::files::Files::new(root.path()),
            std::sync::Arc::new(crate::compaction::Compactions::new(root.path())),
            std::sync::Arc::default(),
        );
        let declared = slot.declaration();
        assert!(declared.steers_room_turns && declared.serializes_room_turns);
        // The tools a room serves reach its turns: delegation (RMK-1532).
        assert!(declared.takes_owner_tools);
        assert_eq!(declared.recent_events_window, crate::compaction::WINDOW);
        assert_eq!(declared.category, ChannelCategory::Intelligence);
        assert!(declared.capabilities.supports_media);
        assert!(
            declared
                .capabilities
                .media_types
                .contains(&ChannelMediaType::Media)
        );
    }
}
