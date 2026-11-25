//! What the profile commands are asked, read once from a command and its
//! arguments, and written by the menus that offer it: one definition for
//! both, so a menu cannot offer what the commands do not understand.

use std::fmt;

/// What `/providers`, `/agents`, `/effort` and `/model` are asked: the
/// command with the structured action a choice carries, or the words a
/// person types. Read once, here: what follows matches on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::profiles) enum Action<'a> {
    /// Asks to confirm the deletion of connection or agent `id`, as
    /// `command` tells which.
    Delete {
        command: &'a str,
        id: &'a str,
    },
    /// The deletion the person confirmed, with the token it was asked with.
    ConfirmDelete {
        command: &'a str,
        token: &'a str,
    },
    /// An action on a ChatGPT account, which `accounts` reads.
    ChatGpt(&'a str),
    /// Saves the connection of a signed-in ChatGPT account.
    UseChatGpt(&'a str),
    EffortMenu,
    Effort(&'a str),
    /// `/model`, with a model's ID or nothing: an ID may hold colons, so it
    /// is the whole argument.
    Model(&'a str),
    Providers,
    OpenProvider(&'a str),
    AddProvider,
    EditProvider(&'a str),
    AddSubscription(&'a str),
    TestProvider(&'a str),
    /// A new agent on `provider`: on the listed `model`, or on one to type
    /// when it is empty.
    NewAgentOn {
        provider: &'a str,
        model: &'a str,
        typed: bool,
    },
    Agents,
    NewAgent(&'a str),
    AgentModels(&'a str),
    UseModel {
        agent: &'a str,
        model: &'a str,
    },
    OpenAgent(&'a str),
    AddAgent,
    EditAgent(&'a str),
    SelectAgent(&'a str),
}

impl<'a> Action<'a> {
    /// The action `arguments` name for `command`. `list` and `delete <id>`
    /// are the words a person types for the structured `` and `delete:<id>`.
    pub(in crate::profiles) fn parse(command: &'a str, arguments: &'a str) -> Result<Self, String> {
        let arguments = arguments.trim();
        let listed = matches!(command, "agents" | "providers");
        if command == "model" {
            return Ok(Self::Model(arguments));
        }
        if listed {
            if let Some(token) = arguments.strip_prefix("confirm-delete:") {
                return Ok(Self::ConfirmDelete { command, token });
            }
            let typed = arguments.strip_prefix("delete ").map(str::trim);
            if let Some(id) = arguments.strip_prefix("delete:").or(typed) {
                return Ok(Self::Delete { command, id });
            }
        }
        if command == "providers" {
            if let Some(action) = arguments.strip_prefix("chatgpt:") {
                return Ok(Self::ChatGpt(action));
            }
            if let Some(account) = arguments.strip_prefix("chatgpt-use:") {
                return Ok(Self::UseChatGpt(
                    account.split(':').next().unwrap_or_default(),
                ));
            }
        }
        let mut parts = arguments.splitn(3, ':');
        let action = parts.next().unwrap_or_default();
        let id = parts.next().unwrap_or_default();
        let model = parts.next().unwrap_or_default();
        Ok(match (command, action) {
            ("effort", "") => Self::EffortMenu,
            ("effort", effort) => Self::Effort(effort),
            ("providers", "" | "list") => Self::Providers,
            ("providers", "open") => Self::OpenProvider(id),
            ("providers", "add") => Self::AddProvider,
            ("providers", "edit") => Self::EditProvider(id),
            ("providers", "subscription") => Self::AddSubscription(id),
            ("providers", "test") => Self::TestProvider(id),
            ("providers", "model" | "typed") => Self::NewAgentOn {
                provider: id,
                model,
                typed: action == "typed",
            },
            ("agents", "" | "list") => Self::Agents,
            ("agents", "new") => Self::NewAgent(id),
            ("agents", "models") => Self::AgentModels(id),
            ("agents", "use-model") => Self::UseModel { agent: id, model },
            ("agents", "open") => Self::OpenAgent(id),
            ("agents", "add") => Self::AddAgent,
            ("agents", "edit") => Self::EditAgent(id),
            ("agents", "select") => Self::SelectAgent(id),
            _ => return Err("Unknown profile action; reopen /providers or /agents".into()),
        })
    }

    /// Whether the action writes the profile files or what a conversation
    /// keeps: it then holds the cross-process edit lease. Menus, forms being
    /// opened and model discovery do not.
    pub(in crate::profiles) fn mutates(&self) -> bool {
        match self {
            Self::SelectAgent(_) | Self::Effort(_) => true,
            Self::Model(id) => !id.is_empty(),
            _ => false,
        }
    }
}

#[cfg(test)]
impl Action<'_> {
    /// The command the action belongs to, which reads it back.
    fn command(&self) -> &str {
        match self {
            Self::Delete { command, .. } | Self::ConfirmDelete { command, .. } => command,
            Self::EffortMenu | Self::Effort(_) => "effort",
            Self::Model(_) => "model",
            Self::ChatGpt(_)
            | Self::UseChatGpt(_)
            | Self::Providers
            | Self::OpenProvider(_)
            | Self::AddProvider
            | Self::EditProvider(_)
            | Self::AddSubscription(_)
            | Self::TestProvider(_)
            | Self::NewAgentOn { .. } => "providers",
            Self::Agents
            | Self::NewAgent(_)
            | Self::AgentModels(_)
            | Self::UseModel { .. }
            | Self::OpenAgent(_)
            | Self::AddAgent
            | Self::EditAgent(_)
            | Self::SelectAgent(_) => "agents",
        }
    }
}

impl fmt::Display for Action<'_> {
    /// The action as a choice of its command carries it, which
    /// [`Action::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Delete { id, .. } => write!(f, "delete:{id}"),
            Self::ConfirmDelete { token, .. } => write!(f, "confirm-delete:{token}"),
            Self::ChatGpt(action) => write!(f, "chatgpt:{action}"),
            Self::UseChatGpt(account) => write!(f, "chatgpt-use:{account}"),
            Self::EffortMenu | Self::Providers | Self::Agents => Ok(()),
            Self::Effort(text) | Self::Model(text) => f.write_str(text),
            Self::OpenProvider(id) | Self::OpenAgent(id) => write!(f, "open:{id}"),
            Self::AddProvider | Self::AddAgent => f.write_str("add"),
            Self::EditProvider(id) | Self::EditAgent(id) => write!(f, "edit:{id}"),
            Self::AddSubscription(id) => write!(f, "subscription:{id}"),
            Self::TestProvider(id) => write!(f, "test:{id}"),
            Self::NewAgentOn {
                provider,
                typed: true,
                ..
            } => write!(f, "typed:{provider}"),
            Self::NewAgentOn {
                provider, model, ..
            } => write!(f, "model:{provider}:{model}"),
            Self::NewAgent(id) => write!(f, "new:{id}"),
            Self::AgentModels(id) => write!(f, "models:{id}"),
            Self::UseModel { agent, model } => write!(f, "use-model:{agent}:{model}"),
            Self::SelectAgent(id) => write!(f, "select:{id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The menus write actions and the commands read them: both are this
    /// one definition, so each action comes back as it was written.
    #[test]
    fn every_action_is_read_back_as_it_was_written() -> Result<(), String> {
        let actions = [
            Action::Delete {
                command: "providers",
                id: "local",
            },
            Action::ConfirmDelete {
                command: "agents",
                token: "0b5c",
            },
            Action::ChatGpt("account:0b5c"),
            Action::UseChatGpt("0b5c"),
            Action::EffortMenu,
            Action::Effort("high"),
            Action::Model("vendor/model:latest"),
            Action::Model(""),
            Action::Providers,
            Action::OpenProvider("local"),
            Action::AddProvider,
            Action::EditProvider("local"),
            Action::AddSubscription("opencode-go"),
            Action::TestProvider("local"),
            Action::NewAgentOn {
                provider: "local",
                model: "vendor/model:latest",
                typed: false,
            },
            Action::NewAgentOn {
                provider: "local",
                model: "",
                typed: true,
            },
            Action::Agents,
            Action::NewAgent("local"),
            Action::AgentModels("code"),
            Action::UseModel {
                agent: "code",
                model: "vendor/model:latest",
            },
            Action::OpenAgent("code"),
            Action::AddAgent,
            Action::EditAgent("code"),
            Action::SelectAgent("code"),
        ];
        for action in actions {
            let written = action.to_string();
            let read = Action::parse(action.command(), &written)?;
            assert_eq!(read, action, "/{} {written}", action.command());
        }
        Ok(())
    }

    #[test]
    fn a_person_s_words_name_the_same_actions() -> Result<(), String> {
        assert_eq!(Action::parse("agents", "list")?, Action::Agents);
        let typed = Action::parse("providers", "delete  local ")?;
        let expected = Action::Delete {
            command: "providers",
            id: "local",
        };
        assert_eq!(typed, expected);
        assert!(Action::parse("agents", "promote:code").is_err());
        assert!(Action::parse("agents", "select:code")?.mutates());
        assert!(!Action::parse("agents", "open:code")?.mutates());
        Ok(())
    }
}
