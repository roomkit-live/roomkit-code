//! A conversation's display name, kept in its room's private state. The ID
//! stays the conversation's identity: `/resume ID`, `--session` and the
//! resume command at exit use it.

use roomkit::model::RoomId;
use roomkit::{ConversationStore, RoomKit};
use roomkit_sqlite::SqliteStore;
use roomkit_tui::protocol::Event;
use roomkit_tui_engine::{CommandResult, SessionConfig};

use super::{Sessions, rooms::empty, title};

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
    if name.chars().any(|c| c.is_control() || reorders(c)) {
        return Err("A conversation's name is a single line of plain text".into());
    }
    if name.chars().all(|c| c.is_whitespace() || zero_width(c)) {
        return Err("A conversation's name needs a visible character".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!(
            "A conversation's name has at most {MAX_NAME} characters"
        ));
    }
    Ok(name.into())
}

/// Bidirectional controls and line or paragraph separators: they could
/// reverse the ID printed after a name at exit, or break its line.
fn reorders(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{2028}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Characters that draw nothing. Joiners belong inside emoji sequences
/// and some scripts' words; a name of them only would show blank.
fn zero_width(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
}

impl Sessions {
    /// `/rename`: name this conversation, also while a response runs: the
    /// view renames it in place. An empty one has nothing to name yet: it is
    /// reused as the next draft and by `/new`.
    pub(super) async fn rename(
        &self,
        current: SessionConfig,
        arguments: &str,
    ) -> Result<CommandResult, String> {
        let name = parse(arguments)?;
        // A task's room belongs to its conversation, the one /resume lists.
        let root = self.profiles.resources.tasks.root_of(current.room.as_str());
        let room = RoomId::new(&root).map_err(|_| "This conversation cannot be named")?;
        if empty(&self.kit, &room)
            .await
            .map_err(|_| "Could not inspect the current session".to_string())?
        {
            return Err("Send a first message before naming this conversation".into());
        }
        set(&self.kit, &room, &name).await?;
        let conversation = current.room.as_str().into();
        Ok(CommandResult::Events(vec![if room == current.room {
            Event::TitleChanged {
                conversation,
                title: title(&self.workspace, &room, Some(&name)),
            }
        } else {
            Event::Notice {
                conversation,
                text: format!("Conversation renamed: {name}"),
            }
        }]))
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
        for refused in [
            "\u{200B}",
            "\u{200D}\u{FEFF} ",
            "a\u{202E}b",
            "a\u{2028}b",
            "\u{2066}a",
            "a\u{061C}b",
        ] {
            assert!(parse(refused).is_err(), "{refused:?}");
        }
        // Joiners inside an emoji sequence or a word are part of the name.
        let persian = "\u{0646}\u{06CC}\u{0645}\u{200C}\u{0641}\u{0627}\u{0635}\u{0644}\u{0647}";
        for kept in ["\u{1F469}\u{200D}\u{1F4BB} review", persian] {
            assert_eq!(parse(kept).as_deref(), Ok(kept), "{kept:?}");
        }
        assert!(parse(&"x".repeat(81)).is_err());
    }
}
