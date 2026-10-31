//! Stable engine identity, with an immutable configuration pinned for each turn.
use roomkit::model::{
    ChannelBinding, ChannelCategory, ChannelDirection, ChannelId, ChannelOutput, ChannelType,
    EventType, InboundMessage, RoomContext, RoomEvent, TextContent,
};
use roomkit::{Channel, ChannelDeclaration, ChannelError, EventDraft};
use roomkit_openai::OpenAiAgent;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub(super) struct AgentSlot {
    policy: crate::permissions::Policy,
    declaration: ChannelDeclaration,
    current: Arc<RwLock<Option<Arc<OpenAiAgent>>>>,
}
impl AgentSlot {
    pub fn new(
        id: ChannelId,
        agent: Option<OpenAiAgent>,
        policy: crate::permissions::Policy,
    ) -> Self {
        let mut declaration = ChannelDeclaration::new(
            id,
            ChannelType::AI,
            ChannelCategory::Intelligence,
            ChannelDirection::Bidirectional,
        );
        // Stable transport accepts media; each turn selects an opt-in vision provider.
        declaration
            .capabilities
            .media_types
            .insert(roomkit::model::ChannelMediaType::Media);
        declaration.capabilities.supports_media = true;
        Self {
            policy,
            declaration,
            current: Arc::new(RwLock::new(agent.map(Arc::new))),
        }
    }
    pub fn replace(&self, agent: OpenAiAgent) {
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(agent));
    }
    pub fn disable(&self) {
        *self
            .current
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}
impl Channel for AgentSlot {
    fn declaration(&self) -> ChannelDeclaration {
        self.declaration.clone()
    }
    async fn handle_inbound(
        &self,
        _: InboundMessage,
        _: RoomContext,
        _: EventDraft,
    ) -> Result<RoomEvent, ChannelError> {
        Err("AI channel does not accept inbound messages".into())
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
            .is_some_and(|ids| ids.contains(&self.declaration.channel_id))
        {
            return Ok(ChannelOutput::default());
        }
        let agent = self
            .current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            let mut input = RoomEvent::new(
                event.room_id.clone(),
                event.source.clone(),
                TextContent::new(self.policy.instruction(event.room_id.as_str())),
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
    async fn close(&self) {
        let agent = self
            .current
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(agent) = agent {
            agent.close().await;
        }
    }
}
