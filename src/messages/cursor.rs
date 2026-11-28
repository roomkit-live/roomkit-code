//! Durable inbox progress for local deliveries and deliveries that need the session group.
use super::Kit;
use roomkit::{ConversationStore, model::RoomId};
use serde_json::json;

const KEY: &str = "roomkit_code:session_mail_cursor:v1";

#[derive(Default)]
pub(super) struct Cursor {
    all: Option<u64>,
    local: Option<u64>,
    local_only: bool,
}

impl Cursor {
    pub async fn load(kit: &Kit, room: &RoomId, local_only: bool) -> crate::Result<Self> {
        let state = kit.store().get_private_state(room, KEY).await?;
        let index = |key| state.as_ref()?.get(key)?.as_u64();
        let all = index("index");
        // Older cursors advanced both routes together.
        let local = index("local_index").max(all);
        Ok(Self {
            all,
            local,
            local_only,
        })
    }

    pub fn after(&self) -> Option<u64> {
        if self.local_only {
            self.local
        } else {
            self.all
        }
    }

    pub fn pending(&self, remote: bool, index: u64) -> bool {
        if remote {
            !self.local_only
        } else {
            Some(index) > self.local
        }
    }

    pub async fn advance(&mut self, kit: &Kit, room: &RoomId, index: u64) -> crate::Result<()> {
        self.local = self.local.max(Some(index));
        if !self.local_only {
            self.all = Some(index);
        }
        let state = [
            ("index".into(), json!(self.all)),
            ("local_index".into(), json!(self.local)),
        ];
        kit.store()
            .set_private_state(room, KEY, Some(state.into_iter().collect()))
            .await?;
        Ok(())
    }
}
