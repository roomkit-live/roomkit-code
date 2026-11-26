//! What the ChatGPT account menus are asked under `/providers`: written by
//! their choices and read back once, in one definition, so a menu cannot
//! offer what the accounts do not understand.

use std::fmt;

/// One action on the ChatGPT accounts, as `/providers` carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action<'a> {
    /// The list of accounts, with a sign-in under way closed.
    Menu,
    /// Signs a new account in.
    Connect,
    /// One account's menu.
    Account(&'a str),
    /// Signs account `id` in again.
    Login(&'a str),
    Logout(&'a str),
    /// Closes sign-in panel `id`, then shows the accounts.
    Cancel(&'a str),
    /// Closes sign-in panel `id`.
    Dismiss(&'a str),
    /// Saves the connection of signed-in account `id`: the profiles serve
    /// it, not the accounts.
    Use(&'a str),
}

impl<'a> Action<'a> {
    /// The action `arguments`, a `/providers` argument, name, if they name
    /// one of the accounts'.
    pub(crate) fn parse(arguments: &'a str) -> Option<Result<Self, String>> {
        if let Some(account) = arguments.strip_prefix("chatgpt-use:") {
            return Some(Ok(Self::Use(account.split(':').next().unwrap_or_default())));
        }
        let action = arguments.strip_prefix("chatgpt:")?;
        let (action, id) = action.split_once(':').unwrap_or((action, ""));
        Some(Ok(match action {
            "menu" | "" => Self::Menu,
            "connect" => Self::Connect,
            "account" => Self::Account(id),
            "login" => Self::Login(id),
            "logout" => Self::Logout(id),
            "cancel" => Self::Cancel(id),
            "dismiss" => Self::Dismiss(id),
            _ => {
                return Some(Err(
                    "Unknown ChatGPT account action; reopen /providers".into()
                ));
            }
        }))
    }
}

impl fmt::Display for Action<'_> {
    /// The action as a choice carries it, which [`Action::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Menu => f.write_str("chatgpt:menu"),
            Self::Connect => f.write_str("chatgpt:connect"),
            Self::Account(id) => write!(f, "chatgpt:account:{id}"),
            Self::Login(id) => write!(f, "chatgpt:login:{id}"),
            Self::Logout(id) => write!(f, "chatgpt:logout:{id}"),
            Self::Cancel(id) => write!(f, "chatgpt:cancel:{id}"),
            Self::Dismiss(id) => write!(f, "chatgpt:dismiss:{id}"),
            Self::Use(id) => write!(f, "chatgpt-use:{id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action, one index each: a variant added without being listed
    /// here does not compile, and one listed is read back as written.
    fn index(action: Action<'_>) -> usize {
        match action {
            Action::Menu => 0,
            Action::Connect => 1,
            Action::Account(_) => 2,
            Action::Login(_) => 3,
            Action::Logout(_) => 4,
            Action::Cancel(_) => 5,
            Action::Dismiss(_) => 6,
            Action::Use(_) => 7,
        }
    }

    #[test]
    fn every_action_is_read_back_as_it_was_written() -> Result<(), String> {
        let id = "3f2a6c1e-account";
        let actions = [
            Action::Menu,
            Action::Connect,
            Action::Account(id),
            Action::Login(id),
            Action::Logout(id),
            Action::Cancel(id),
            Action::Dismiss(id),
            Action::Use(id),
        ];
        let mut listed: Vec<usize> = actions.iter().map(|a| index(*a)).collect();
        listed.sort_unstable();
        assert_eq!(listed, (0..actions.len()).collect::<Vec<_>>());
        for action in actions {
            let written = action.to_string();
            assert_eq!(Action::parse(&written), Some(Ok(action)), "{written}");
        }
        assert_eq!(Action::parse("chatgpt:"), Some(Ok(Action::Menu)));
        assert!(matches!(Action::parse("chatgpt:revoke:x"), Some(Err(_))));
        assert_eq!(Action::parse("open:other"), None);
        Ok(())
    }
}
