//! The actions of the application's panels: `<panel>:<verb>`.
//!
//! A panel's choices carry the panel's own identifier, so an action sent
//! from an older view is told from one of the view now open. The plans, the
//! processes and the tasks each wrote and read this form on their own: it
//! is written and read here, once, with the three verbs they share. What a
//! panel alone understands (`follow`, `stop`, a page of output…) is its
//! [`Verb::Own`].

use std::fmt;

/// What a panel is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verb<'a> {
    /// Closes the panel.
    Dismiss,
    /// Back to the panel's list.
    List,
    /// One entry of the list, by its identifier.
    Open(&'a str),
    /// A verb of the panel's own.
    Own(&'a str),
}

impl<'a> Verb<'a> {
    fn parse(text: &'a str) -> Self {
        match text {
            "dismiss" => Self::Dismiss,
            "list" => Self::List,
            _ => match text.strip_prefix("open:") {
                Some(id) => Self::Open(id),
                None => Self::Own(text),
            },
        }
    }

    /// The action as a choice of panel `panel` carries it, which
    /// [`addressed`] reads back.
    pub(crate) fn to(self, panel: &str) -> String {
        format!("{panel}:{self}")
    }
}

impl fmt::Display for Verb<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dismiss => f.write_str("dismiss"),
            Self::List => f.write_str("list"),
            Self::Open(id) => write!(f, "open:{id}"),
            Self::Own(verb) => f.write_str(verb),
        }
    }
}

/// The panel `arguments` address and what they ask of it; none for
/// arguments that address no panel, which a command reads as its own.
pub(crate) fn addressed(arguments: &str) -> Option<(&str, Verb<'_>)> {
    let (panel, verb) = arguments.split_once(':')?;
    Some((panel, Verb::parse(verb)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_is_read_back_as_it_was_written() {
        let verbs = [
            Verb::Dismiss,
            Verb::List,
            Verb::Open("p3"),
            Verb::Own("follow"),
            Verb::Own("room:task-1"),
        ];
        for verb in verbs {
            let written = verb.to("0b5c");
            assert_eq!(addressed(&written), Some(("0b5c", verb)), "{written}");
        }
        assert_eq!(addressed("p3"), None);
        assert_eq!(addressed(""), None);
    }
}
