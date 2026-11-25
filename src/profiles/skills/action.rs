//! What `/skills` is asked: the action a menu's choice carries, or the words
//! a person types. One definition writes an action and reads it back.

use std::fmt;

/// One action of `/skills`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::profiles) enum Action<'a> {
    List,
    Close,
    /// Reads the skill directories again.
    Reload,
    /// Where skills are read from, and what reading them reported.
    Locations,
    /// One skill's description and its actions.
    Open(&'a str),
    Enable(&'a str),
    Disable(&'a str),
    /// Has the agent load the skill, then do `task`, or introduce the skill
    /// without one.
    Use {
        name: &'a str,
        task: Option<&'a str>,
    },
}

/// What a request that names no action of `/skills` is told.
const USAGE: &str = "Use /skills [list|reload|enable <name>|disable <name>|use <name> <task>]";

impl<'a> Action<'a> {
    /// The action `arguments` name. A word alone that is no action is a
    /// skill's name, to open.
    pub(in crate::profiles) fn parse(arguments: &'a str) -> Result<Self, String> {
        let arguments = arguments.trim();
        match arguments {
            "" | "list" => return Ok(Self::List),
            "close" => return Ok(Self::Close),
            "reload" => return Ok(Self::Reload),
            "locations" => return Ok(Self::Locations),
            _ => {}
        }
        let Some((action, rest)) = arguments.split_once([':', ' ']) else {
            return Ok(Self::Open(arguments));
        };
        Ok(match action {
            "open" => Self::Open(rest),
            "enable" => Self::Enable(rest),
            "disable" => Self::Disable(rest),
            "use" => {
                let (name, task) = match rest.split_once(' ') {
                    Some((name, task)) => (name, Some(task)),
                    None => (rest, None),
                };
                Self::Use { name, task }
            }
            _ => return Err(USAGE.into()),
        })
    }

    /// Whether the action writes the profile files or replaces the skills
    /// read: it then holds the cross-process edit lease, which a menu left
    /// open must not.
    pub(in crate::profiles) fn mutates(&self) -> bool {
        matches!(self, Self::Reload | Self::Enable(_) | Self::Disable(_))
    }
}

impl fmt::Display for Action<'_> {
    /// The action as a choice carries it, which [`Action::parse`] reads back.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::List => f.write_str("list"),
            Self::Close => f.write_str("close"),
            Self::Reload => f.write_str("reload"),
            Self::Locations => f.write_str("locations"),
            Self::Open(name) => write!(f, "open:{name}"),
            Self::Enable(name) => write!(f, "enable:{name}"),
            Self::Disable(name) => write!(f, "disable:{name}"),
            Self::Use { name, task: None } => write!(f, "use:{name}"),
            Self::Use {
                name,
                task: Some(task),
            } => write!(f, "use:{name} {task}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_is_read_back_as_it_was_written() -> Result<(), String> {
        let actions = [
            Action::List,
            Action::Close,
            Action::Reload,
            Action::Locations,
            Action::Open("review"),
            Action::Enable("review"),
            Action::Disable("review"),
            Action::Use {
                name: "review",
                task: None,
            },
            Action::Use {
                name: "review",
                task: Some("Check the last commit"),
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
        assert_eq!(Action::parse("  ")?, Action::List);
        assert_eq!(Action::parse("review")?, Action::Open("review"));
        assert_eq!(Action::parse("enable review")?, Action::Enable("review"));
        let asked = Action::parse("use review Check the tests")?;
        let expected = Action::Use {
            name: "review",
            task: Some("Check the tests"),
        };
        assert_eq!(asked, expected);
        assert!(Action::parse("install review").is_err());
        assert!(Action::parse("reload")?.mutates() && !Action::parse("list")?.mutates());
        Ok(())
    }
}
