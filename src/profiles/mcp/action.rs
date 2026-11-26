//! What `/mcp` is asked: the action a menu's choice carries, or the words a
//! person types. One definition writes an action and reads it back, so a
//! menu cannot offer what the command does not understand.

use std::fmt;

/// One action of `/mcp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::profiles) enum Action<'a> {
    List,
    Close,
    /// The form of a new stdio server.
    Add,
    /// The form of a new HTTP server.
    AddHttp,
    Show(&'a str),
    /// A server's tools, from the tool at `page`.
    Tools {
        id: &'a str,
        page: usize,
    },
    Edit(&'a str),
    Delete(&'a str),
    /// The deletion the person confirmed, with the token it was asked with.
    ConfirmDelete {
        id: &'a str,
        token: &'a str,
    },
    Connect(&'a str),
    Disconnect(&'a str),
    /// Every tool of the server, for the current agent.
    Enable(&'a str),
    /// None of them.
    Disable(&'a str),
    /// One tool, on or off, then the tools from `page` again.
    Toggle {
        id: &'a str,
        tool: &'a str,
        page: usize,
    },
}

/// What a request that names no action of `/mcp` is told.
const USAGE: &str = "Use /mcp to configure servers, connect and select tools for this agent";

impl<'a> Action<'a> {
    /// The action `arguments` name: words separated by colons, as a choice
    /// writes them, or by spaces, as a person types them.
    pub(in crate::profiles) fn parse(arguments: &'a str) -> Result<Self, String> {
        // As before the actions had a type: a tab or a newline typed around
        // them is no part of them.
        let arguments = arguments.trim();
        let mut parts = arguments.split([':', ' ']).filter(|part| !part.is_empty());
        let action = parts.next().unwrap_or("list");
        let id = parts.next().unwrap_or_default();
        let third = parts.next();
        let page = |text: Option<&str>| text.and_then(|page| page.parse().ok()).unwrap_or(0);
        Ok(match action {
            "list" => Self::List,
            "close" => Self::Close,
            "add" => Self::Add,
            "add-http" => Self::AddHttp,
            "show" => Self::Show(id),
            "tools" => Self::Tools {
                id,
                page: page(third),
            },
            "edit" => Self::Edit(id),
            "delete" => Self::Delete(id),
            "confirm-delete" => Self::ConfirmDelete {
                id,
                token: third.unwrap_or_default(),
            },
            "connect" => Self::Connect(id),
            "disconnect" => Self::Disconnect(id),
            "enable" => Self::Enable(id),
            "disable" => Self::Disable(id),
            "toggle" => Self::Toggle {
                id,
                tool: third.ok_or("Choose an MCP tool")?,
                page: page(parts.next()),
            },
            _ => return Err(USAGE.into()),
        })
    }

    /// Whether the action writes the profile files: it then holds the
    /// cross-process edit lease, which a menu left open must not. Every
    /// action is named, so that a new one cannot skip the lease unnoticed.
    pub(in crate::profiles) fn mutates(&self) -> bool {
        match self {
            Self::ConfirmDelete { .. }
            | Self::Enable(_)
            | Self::Disable(_)
            | Self::Toggle { .. } => true,
            // A connection is the runtime's, which takes the lease to save.
            Self::Connect(_) | Self::Disconnect(_) => false,
            Self::List
            | Self::Close
            | Self::Add
            | Self::AddHttp
            | Self::Show(_)
            | Self::Tools { .. }
            | Self::Edit(_)
            | Self::Delete(_) => false,
        }
    }
}

impl fmt::Display for Action<'_> {
    /// The action as a choice carries it, which [`Action::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List => f.write_str("list"),
            Self::Close => f.write_str("close"),
            Self::Add => f.write_str("add"),
            Self::AddHttp => f.write_str("add-http"),
            Self::Show(id) => write!(f, "show:{id}"),
            Self::Tools { id, page } => write!(f, "tools:{id}:{page}"),
            Self::Edit(id) => write!(f, "edit:{id}"),
            Self::Delete(id) => write!(f, "delete:{id}"),
            Self::ConfirmDelete { id, token } => write!(f, "confirm-delete:{id}:{token}"),
            Self::Connect(id) => write!(f, "connect:{id}"),
            Self::Disconnect(id) => write!(f, "disconnect:{id}"),
            Self::Enable(id) => write!(f, "enable:{id}"),
            Self::Disable(id) => write!(f, "disable:{id}"),
            Self::Toggle { id, tool, page } => write!(f, "toggle:{id}:{tool}:{page}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The menus write actions and the command reads them: both are this
    /// one definition, so each action comes back as it was written.
    #[test]
    fn every_action_is_read_back_as_it_was_written() -> Result<(), String> {
        let actions = [
            Action::List,
            Action::Close,
            Action::Add,
            Action::AddHttp,
            Action::Show("files"),
            Action::Tools {
                id: "files",
                page: 30,
            },
            Action::Edit("files"),
            Action::Delete("files"),
            Action::ConfirmDelete {
                id: "files",
                token: "0b5c",
            },
            Action::Connect("files"),
            Action::Disconnect("files"),
            Action::Enable("files"),
            Action::Disable("files"),
            Action::Toggle {
                id: "files",
                tool: "read_file",
                page: 60,
            },
        ];
        for action in actions {
            let written = action.to_string();
            assert_eq!(Action::parse(&written)?, action, "{written}");
        }
        Ok(())
    }

    #[test]
    fn a_person_s_words_name_the_same_actions() -> Result<(), String> {
        assert_eq!(Action::parse("")?, Action::List);
        assert_eq!(Action::parse("connect files")?, Action::Connect("files"));
        assert_eq!(Action::parse(" disable  files ")?, Action::Disable("files"));
        // Review finding: since 7d6ea96 a tab or a newline typed around an
        // action gave the usage text.
        assert_eq!(
            Action::parse("\tconnect files\n")?,
            Action::Connect("files")
        );
        assert!(Action::parse("toggle files").is_err());
        assert!(Action::parse("uninstall files").is_err());
        assert!(Action::parse("toggle:files:read").is_ok_and(|action| action.mutates()));
        assert!(!Action::parse("show:files")?.mutates());
        Ok(())
    }
}
