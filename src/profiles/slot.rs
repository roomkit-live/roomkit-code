//! Stable engine identity, with an immutable configuration pinned for each turn.
use roomkit::model::{
    ChannelBinding, ChannelCapabilities, ChannelCategory, ChannelId, ChannelMediaType,
    ChannelOutput, ChannelType, EventType, InboundMessage, RoomContext, RoomEvent, TextContent,
};
use roomkit::{Channel, ChannelError};
use std::sync::{Arc, RwLock};

use crate::agent::CodeAgent;

#[derive(Clone)]
pub(super) struct AgentSlot {
    instructions: Arc<crate::instructions::Instructions>,
    files: Arc<crate::files::Files>,
    policy: crate::permissions::Policy,
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
    ) -> Self {
        Self {
            instructions,
            files,
            policy,
            id,
            current: Arc::new(RwLock::new(agent.map(Arc::new))),
        }
    }
    pub fn replace(&self, agent: CodeAgent) {
        *crate::sync::write(&self.current) = Some(Arc::new(agent));
    }
    pub fn disable(&self) {
        *crate::sync::write(&self.current) = None;
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
    /// The history each agent's turn reads: the kit sizes the context it
    /// hands the slot by it, as it reads the slot's declaration once, when
    /// the slot is registered.
    fn recent_events_window(&self) -> usize {
        crate::agent::HISTORY
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
        if !event
            .addressed_to
            .as_ref()
            .is_some_and(|ids| ids.contains(&self.id))
        {
            return Ok(ChannelOutput::default());
        }
        let agent = crate::sync::read(&self.current)
            .clone()
            .ok_or("Configure a provider and agent first")?;
        // Keep the delivered user event intact, including images and its durable
        // identity. The delegated channel receives an application Instruction
        // after it, in this same turn. It is never committed as a user's words or
        // inserted into the system prompt/tool schemas. The engine's transcript
        // renderer distinguishes application instructions from participant text.
        let input = if event.r#type == EventType::Message {
            if !context.recent_events.iter().any(|past| past.id == event.id) {
                context.recent_events.push(event.clone());
            }
            self.instructions
                .begin(event.room_id.as_str(), self.id.as_str());
            let project = self.instructions.load(vec![], true).await;
            let mut input = RoomEvent::new(
                event.room_id.clone(),
                event.source.clone(),
                TextContent::new(format!(
                    "{}\n\n{}",
                    self.policy.instruction(event.room_id.as_str()),
                    project.render()
                )),
            );
            input.r#type = EventType::Instruction;
            input.addressed_to = event.addressed_to.clone();
            input.index = event.index;
            input
        } else {
            event
        };
        agent.on_event(input, binding, context).await
    }
    /// Records what the shell calls the kit's close cut changed, while the
    /// kit's store is still open: it closes the store after its channels.
    /// Then closes the current agent's provider; the host closes what serves
    /// its tools.
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
    /// slot declares its agents' history, whatever agent it holds then.
    #[test]
    fn the_slot_declares_its_agents_history_and_media() {
        let root = tempfile::tempdir().unwrap();
        let slot = AgentSlot::new(
            ChannelId::new("code").unwrap(),
            None,
            crate::permissions::Policy::new(crate::permissions::Mode::Ask),
            crate::instructions::Instructions::new(root.path(), root.path()),
            crate::files::Files::new(root.path()),
        );
        let declared = slot.declaration();
        assert_eq!(declared.recent_events_window, crate::agent::HISTORY);
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
