//! The names several parts of the application know each other by: the
//! channels and the tools that are named across modules.
//!
//! They live here, in a module that depends on nothing, so that naming a
//! tool or the terminal's channel does not make a module depend on the
//! whole module that serves it: most of the application's modules used to
//! form one cycle through these few constants.

/// The channel of the application's views, the terminal's: what a row the
/// application writes for its views alone comes from.
pub(crate) const TERMINAL: &str = "terminal";

/// The agent that stands for the remote worker, or the pool's workers.
pub(crate) const REMOTE_AGENT: &str = "remote-linux";

/// The tool an agent asks the person a question with.
pub(crate) const ASK_USER: &str = "ask_user";

/// The tools of the correspondence between sessions.
pub(crate) const SEND_MESSAGE: &str = "send_message";
pub(crate) const LIST_SESSIONS: &str = "list_sessions";

/// The executor pool's tools: its inventory, a commit's tests on a worker,
/// and the consultation between the tasks of one conversation.
pub(crate) const LIST_EXECUTORS: &str = "list_executors";
pub(crate) const CARGO_TEST: &str = "cargo_test";
pub(crate) const ASK_PEER: &str = "ask_peer";
pub(crate) const ANSWER_PEER: &str = "answer_peer";
