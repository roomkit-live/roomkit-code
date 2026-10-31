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
    instructions: Arc<crate::instructions::Instructions>,
    policy: crate::permissions::Policy,
    declaration: ChannelDeclaration,
    current: Arc<RwLock<Option<Arc<OpenAiAgent>>>>,
}
impl AgentSlot {
    pub fn new(
        id: ChannelId,
        agent: Option<OpenAiAgent>,
        policy: crate::permissions::Policy,
        instructions: Arc<crate::instructions::Instructions>,
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
            instructions,
            policy,
            declaration,
            current: Arc::new(RwLock::new(agent.map(Arc::new))),
        }
    }
    pub fn replace(&self, agent: OpenAiAgent) {
        *crate::sync::write(&self.current) = Some(Arc::new(agent));
    }
    pub fn disable(&self) {
        *crate::sync::write(&self.current) = None;
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
                .begin(event.room_id.as_str(), self.declaration.channel_id.as_str());
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
    async fn close(&self) {
        let agent = crate::sync::read(&self.current).clone();
        if let Some(agent) = agent {
            agent.close().await;
        }
    }
}
