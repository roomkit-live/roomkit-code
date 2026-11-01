//! A conversation's display name, kept in its room's private state. The ID
//! stays the conversation's identity: `/resume ID`, `--session` and the
//! resume command at exit use it.

use roomkit::model::RoomId;
use roomkit::{ConversationStore, RoomKit};
use roomkit_sqlite::SqliteStore;

const KEY: &str = "roomkit_code:name";

/// A name's length, in characters.
const MAX_NAME: usize = 80;

/// The name given to `room`, if any. A store error reads as no name: the
/// session still opens under its ID.
pub(crate) async fn name(kit: &RoomKit<SqliteStore>, room: &RoomId) -> Option<String> {
    let state = kit.store().get_private_state(room, KEY).await.ok()??;
    state
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
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

/// The name `/rename` was given: one line of at most 80 characters.
pub(super) fn parse(arguments: &str) -> Result<String, String> {
    let name = arguments.trim();
    if name.is_empty() {
        return Err("Use /rename <name>".into());
    }
    if name.chars().any(char::is_control) {
        return Err("A conversation's name is a single line of text".into());
    }
    if name.chars().count() > MAX_NAME {
        return Err(format!(
            "A conversation's name has at most {MAX_NAME} characters"
        ));
    }
    Ok(name.into())
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
        assert!(parse(&"x".repeat(81)).is_err());
    }
}
