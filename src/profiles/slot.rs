//! Stable engine identity, with an immutable configuration pinned for each turn.
use roomkit::model::{
    ChannelBinding, ChannelCategory, ChannelDirection, ChannelId, ChannelOutput, ChannelType,
    InboundMessage, RoomContext, RoomEvent,
};
use roomkit::{Channel, ChannelDeclaration, ChannelError, EventDraft};
use roomkit_openai::OpenAiAgent;
use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub(super) struct AgentSlot {
    declaration: ChannelDeclaration,
    current: Arc<RwLock<Option<Arc<OpenAiAgent>>>>,
}
impl AgentSlot {
    pub fn new(id: ChannelId, agent: Option<OpenAiAgent>) -> Self {
        let declaration = ChannelDeclaration::new(
            id,
            ChannelType::AI,
            ChannelCategory::Intelligence,
            ChannelDirection::Bidirectional,
        );
        Self {
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
        context: RoomContext,
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
        agent.on_event(event, binding, context).await
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
