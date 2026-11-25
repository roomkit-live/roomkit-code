//! What the profile commands are asked, read once from a command and its
//! arguments.

/// What `/providers`, `/agents`, `/effort` and `/model` are asked: the
/// command with the structured action a choice carries, or the words a
/// person types. Read once, here: what follows matches on it.
pub(super) enum Action<'a> {
    /// `delete:` and `confirm-delete:`, which `deletion` reads.
    Delete {
        command: &'a str,
        arguments: &'a str,
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
    pub(super) fn parse(command: &'a str, arguments: &'a str) -> Result<Self, String> {
        let arguments = arguments.trim();
        let listed = matches!(command, "agents" | "providers");
        if command == "model" {
            return Ok(Self::Model(arguments));
        }
        if listed
            && (arguments.starts_with("delete:")
                || arguments.starts_with("delete ")
                || arguments.starts_with("confirm-delete:"))
        {
            return Ok(Self::Delete { command, arguments });
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
    pub(super) fn mutates(&self) -> bool {
        match self {
            Self::SelectAgent(_) | Self::Effort(_) => true,
            Self::Model(id) => !id.is_empty(),
            _ => false,
        }
    }
}
