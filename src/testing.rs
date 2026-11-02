//! Test support: one tool call through a real kit.
//!
//! A tool reads the room and the acting agent of its call from RoomKit's
//! current tool call, which only RoomKit sets around a call it runs. These
//! helpers run one call of an agent in a room, as the application's kit
//! would, and return what the tool's handler answered.

#![allow(clippy::expect_used, reason = "A broken fixture fails the test")]

use std::sync::{Arc, Mutex, PoisonError};

use roomkit::model::{ChannelId, ChannelType, InboundMessage, Metadata, RoomId, TextContent};
use roomkit::testing::{RecordingTransport, RecordingTransportOptions};
use roomkit::{
    AttachChannelOptions, CreateRoomOptions, ProcessInboundOptions, RoomKit, RoomKitOptions,
};
use roomkit_ai::testing::{ScriptedProvider, ScriptedRound};
use roomkit_ai::{
    AiChannel, AiChannelOptions, AiTool, Tool, ToolEntry, ToolError, ToolFuture, ToolResult,
};
use serde_json::Value;

/// What a tool's handler answered.
pub(crate) type Answer = Result<ToolResult, ToolError>;

/// `tool`, keeping a copy of what its handler answers.
struct Recording {
    tool: Arc<dyn Tool>,
    answer: Arc<Mutex<Option<Answer>>>,
}

impl Tool for Recording {
    fn definition(&self) -> AiTool {
        self.tool.definition()
    }

    fn handler(&self, name: String, arguments: Metadata) -> ToolFuture {
        let call = self.tool.handler(name, arguments);
        let answer = Arc::clone(&self.answer);
        Box::pin(async move {
            let answered = call.await;
            *answer.lock().unwrap_or_else(PoisonError::into_inner) = Some(answered.clone());
            answered
        })
    }
}

/// What `tool` answered the call of `name` with `arguments`, made by the
/// agent `agent` in the room `room`.
pub(crate) async fn call(
    tool: &Arc<dyn Tool>,
    room: &str,
    agent: &str,
    name: &str,
    arguments: Value,
) -> Answer {
    turn(
        tool,
        room,
        agent,
        name,
        arguments,
        AiChannelOptions::default(),
    )
    .await
    .expect("RoomKit handed the call to the tool")
}

/// The call of `name` with `arguments`, made by the agent `agent` in the
/// room `room`, by an AI channel taking `options` and serving the call with
/// `tool`: its model calls the tool once, then answers. `None` when the
/// tool never answered: RoomKit refused the call first, or dropped it.
pub(crate) async fn turn(
    tool: &Arc<dyn Tool>,
    room: &str,
    agent: &str,
    name: &str,
    arguments: Value,
    options: AiChannelOptions,
) -> Option<Answer> {
    let answer = Arc::new(Mutex::new(None));
    let recording = Recording {
        tool: Arc::clone(tool),
        answer: Arc::clone(&answer),
    };
    let arguments = arguments.to_string();
    let model = ScriptedProvider::new([
        ScriptedRound::calling("", [("call", name, arguments.as_str())]),
        ScriptedRound::answering("Done"),
    ]);
    let options = AiChannelOptions {
        tools: vec![ToolEntry::Tool(Arc::new(recording))],
        ..options
    };
    let agent = ChannelId::new(agent).expect("valid agent id");
    let channel = AiChannel::new(agent.clone(), model, options).expect("valid channel");
    let kit = RoomKit::new(RoomKitOptions::default());
    kit.register_channel(channel).expect("agent registers");
    ask(&kit, &RoomId::new(room).expect("valid room id"), &agent).await;
    kit.close().await.expect("kit closes");
    answer.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Sends one message into `room`, where the registered agent `agent`
/// answers it, then waits until `kit` runs nothing more.
async fn ask(kit: &RoomKit, room: &RoomId, agent: &ChannelId) {
    let chat = RecordingTransport::new(
        ChannelId::new("chat").expect("valid chat id"),
        ChannelType::WEBSOCKET,
        RecordingTransportOptions::default(),
    );
    kit.register_channel(chat.clone()).expect("chat registers");
    kit.create_room(CreateRoomOptions {
        room_id: Some(room.clone()),
        ..Default::default()
    })
    .await
    .expect("room is created");
    for channel in [chat.channel_id(), agent] {
        kit.attach_channel(room, channel, AttachChannelOptions::default())
            .await
            .expect("room attaches the channel");
    }
    let message = InboundMessage::new(
        chat.channel_id().clone(),
        "person",
        TextContent::new("Use the tool."),
    );
    let options = ProcessInboundOptions {
        room_id: Some(room.clone()),
        ..Default::default()
    };
    let result = kit
        .process_inbound(message, options)
        .await
        .expect("inbound message is processed");
    assert!(result.error.is_none(), "{result:?}");
    roomkit::testing::idle(kit).await;
}
