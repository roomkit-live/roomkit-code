//! A typed message has a position in the sender's history before its peer can answer.
use super::record::{Kind, Mail};
use crate::system_row::SystemRow;
use roomkit::model::{ChannelId, EventContent, EventId, RoomEvent, RoomId, TextContent};
use roomkit::{CommitEventOptions, ConversationStore, RoomKit, UpdateEventOptions};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::SessionConfig;
use serde_json::json;

const PENDING: &str = "Delivery unconfirmed · ";

type Kit = RoomKit<SqliteStore>;

pub(super) struct Outgoing {
    row: RoomEvent,
    text: String,
}

impl Outgoing {
    pub async fn record(
        kit: &Kit,
        view: &SessionConfig,
        terminal: &ChannelId,
        to: &RoomId,
        id: &EventId,
        body: &str,
    ) -> crate::Result<Self> {
        let text = format!("Message to @{to}\n{body}");
        let pending = format!("{PENDING}{text}");
        let id = format!("sent-{id}");
        let mut event = SystemRow {
            room: view.room.as_str(),
            id: &id,
            text: &pending,
            about: ("session_recipient", json!(to)),
        }
        .event(terminal)?;
        event.idempotency_key = Some(id);
        event
            .metadata
            .insert("session_delivery".into(), json!("unconfirmed"));
        let row = Self::commit(kit, event, &text, view.organization.clone()).await?;
        Ok(Self { row, text })
    }

    async fn commit(
        kit: &Kit,
        event: RoomEvent,
        text: &str,
        organization: Option<String>,
    ) -> crate::Result<RoomEvent> {
        let options = CommitEventOptions {
            organization_id: organization,
        };
        match kit
            .commit_event(&event.room_id, event.clone(), options)
            .await
        {
            Ok(row) => Ok(row),
            Err(error) => {
                let stored = kit.store().get_event(&event.id).await?;
                stored
                    .filter(|row| {
                        row.room_id == event.room_id
                            && row.source == event.source
                            && row.metadata.get("session_recipient")
                                == event.metadata.get("session_recipient")
                            && (row.content == event.content
                                || row.content == TextContent::new(text).into())
                    })
                    .ok_or_else(|| error.into())
            }
        }
    }

    pub fn confirmed(&self) -> bool {
        self.row.content == TextContent::new(&self.text).into()
    }

    pub async fn confirm(&self, kit: &Kit, organization: Option<String>) -> crate::Result<()> {
        let mut metadata = self.row.metadata.clone();
        metadata.insert("session_delivery".into(), json!("accepted"));
        let options = UpdateEventOptions {
            metadata: Some(metadata),
            content: Some(TextContent::new(&self.text).into()),
            organization_id: organization,
            ..Default::default()
        };
        kit.update_event(&self.row.room_id, &self.row.id, options)
            .await?
            .ok_or("Missing outgoing history row")?;
        Ok(())
    }

    pub fn item(self) -> roomkit_tui::protocol::Event {
        use roomkit_tui::protocol::{ActorId, Event, Item, Message, RowPosition};
        let mut message =
            Message::new(self.row.id.to_string(), ActorId::new("terminal"), self.text);
        message.position = Some(RowPosition::stored(self.row.index, None));
        Event::Item {
            conversation: self.row.room_id.to_string().into(),
            item: Item::Message(message),
        }
    }
}

/// A verified answer also proves receipt if the original acknowledgement or
/// its local write was lost. Agent questions have no typed outgoing row.
pub(super) async fn confirm_return(kit: &Kit, room: &RoomId, mail: &Mail) -> crate::Result<()> {
    let (Kind::Reply(original) | Kind::Notice(original)) = &mail.kind else {
        return Ok(());
    };
    let id = EventId::new(format!("sent-{original}"))?;
    let Some(row) = kit.store().get_event(&id).await? else {
        return Ok(());
    };
    if row.room_id != *room
        || row.metadata.get("session_recipient") != Some(&json!(mail.from))
        || row.metadata.get("session_delivery") != Some(&json!("unconfirmed"))
    {
        return Ok(());
    }
    let EventContent::Text(content) = &row.content else {
        return Err("Invalid outgoing message content".into());
    };
    let text = content
        .body
        .strip_prefix(PENDING)
        .ok_or("Invalid pending message")?
        .to_owned();
    let organization = kit
        .get_room(room, Default::default())
        .await?
        .organization_id;
    Outgoing { row, text }.confirm(kit, organization).await
}
