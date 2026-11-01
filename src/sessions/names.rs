//! A conversation's display name, kept in its room's private state. The ID
//! stays the conversation's identity: `/resume ID`, `--session` and the
//! resume command at exit use it.

use roomkit::model::RoomId;
use roomkit::{ConversationStore, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui_engine::{CommandResult, SessionConfig};

use super::{Sessions, rooms::empty};

const KEY: &str = "roomkit_code:name";

/// A name's length, in characters.
const MAX_NAME: usize = 80;

/// The name given to `room`, if any. A store error, or a stored name this
/// version would refuse, reads as no name: the session opens under its ID.
pub(crate) async fn name(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Option<String> {
    let state = kit.store().get_private_state(room, KEY).await.ok()??;
    let stored = state.get("name").and_then(serde_json::Value::as_str)?;
    parse(stored).ok()
}

pub(super) async fn set(
    kit: &RoomKit<SqliteStore>,
    room: &RoomId,
    name: &str,
) -> Result<(), String> {
    let state = [("name".into(), serde_json::Value::String(name.into()))]
        .into_iter()
        .collect();
    kit.store()
        .set_private_state(room, KEY, Some(state))
        .await
        .map_err(|_| "Could not save the conversation's name".to_string())
}

/// The name `/rename` was given: one visible line of at most 80 characters.
pub(super) fn parse(arguments: &str) -> Result<String, String> {
    let name = arguments.trim();
    if name.is_empty() {
        return Err("Use /rename <name>".into());
    }
    if name.chars().any(|c| c.is_control() || invisible(c)) {
        return Err("A conversation's name is a single line of visible text".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!(
            "A conversation's name has at most {MAX_NAME} characters"
        ));
    }
    Ok(name.into())
}

/// Characters that draw nothing or reorder what follows: zero-width
/// spaces and joiners, line and paragraph separators, bidirectional
/// controls, the byte order mark. A name of them would show blank, and one
/// could reverse the ID printed after it at exit.
fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

impl Sessions {
    /// `/rename`: name this conversation. An empty one has nothing to name
    /// yet: it is reused as the next draft and by `/new`.
    pub(super) async fn rename(
        &self,
        current: SessionConfig,
        arguments: &str,
    ) -> Result<CommandResult, String> {
        let name = parse(arguments)?;
        if empty(&self.kit, &current.room)
            .await
            .map_err(|_| "Could not inspect the current session".to_string())?
        {
            return Err("Send a first message before naming this conversation".into());
        }
        set(&self.kit, &current.room, &name).await?;
        Ok(self.switched(current.clone(), current.room).await)
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn a_name_is_one_trimmed_line_of_at_most_80_characters() {
        assert_eq!(parse("  Release notes  ").as_deref(), Ok("Release notes"));
        assert_eq!(parse(&"é".repeat(80)).map(|n| n.chars().count()), Ok(80));
        assert!(parse("").is_err() && parse("   ").is_err());
        assert!(parse("two\nlines").is_err());
        for invisible in [
            "\u{200B}",
            "a\u{202E}b",
            "a\u{2028}b",
            "\u{2066}a",
            "\u{FEFF}a",
        ] {
            assert!(parse(invisible).is_err(), "{invisible:?}");
        }
        assert!(parse(&"x".repeat(81)).is_err());
    }
}
