//! The session group's key: what proves that a message comes from a member
//! of the group, not from anyone who can write to its broker.
//!
//! The broker carries every message, so it cannot hold what authenticates
//! them: the key is given to each member outside it, in
//! `ROOMKIT_SESSION_GROUP_KEY`. A member that has one seals what it sends
//! and accepts only what is sealed with it. The members of a group trust
//! each other: the key tells them from outsiders, not from one another.

use hmac::{Hmac, KeyInit, Mac};
use roomkit::model::EventId;
use serde_json::{Value, json};
use sha2::Sha256;

/// The environment variable that holds the group's key.
pub(in crate::messages) const VARIABLE: &str = "ROOMKIT_SESSION_GROUP_KEY";
/// The fewest bytes a key holds.
const SHORTEST: usize = 32;

/// A group's key, with the group it seals for.
#[derive(Clone)]
pub(in crate::messages) struct Key {
    secret: Vec<u8>,
    group: String,
}

impl Key {
    /// The key `group`'s members share, when the environment gives one.
    pub(in crate::messages) fn from_environment(group: &str) -> crate::Result<Option<Self>> {
        match std::env::var(VARIABLE) {
            Ok(secret) => Self::new(group, &secret).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(format!("{VARIABLE} is not valid text").into()),
        }
    }

    fn new(group: &str, secret: &str) -> crate::Result<Self> {
        if secret.len() < SHORTEST {
            return Err(format!("{VARIABLE} must hold at least {SHORTEST} characters").into());
        }
        Ok(Self {
            secret: secret.as_bytes().to_vec(),
            group: group.to_owned(),
        })
    }

    /// The authenticator of the message `id` carrying `mail`, in this
    /// key's group: the mail names its sender and its recipient, so a
    /// sealed message cannot be given another of either.
    fn mac(&self, id: &EventId, mail: &Value) -> Option<Hmac<Sha256>> {
        let sealed = serde_json::to_vec(&json!([self.group, id, mail])).ok()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).ok()?;
        mac.update(&sealed);
        Some(mac)
    }

    /// The seal a message leaves with.
    pub(in crate::messages) fn seal(&self, id: &EventId, mail: &Value) -> crate::Result<String> {
        let mac = self.mac(id, mail).ok_or("Cannot seal the message")?;
        Ok(crate::digest::hex(&mac.finalize().into_bytes()))
    }

    /// Whether `seal` is the one this key gives the message, compared
    /// without telling how much of it matched.
    fn opens(&self, id: &EventId, mail: &Value, seal: &str) -> bool {
        let Some(given) = unhex(seal) else {
            return false;
        };
        self.mac(id, mail)
            .is_some_and(|mac| mac.verify_slice(&given).is_ok())
    }
}

/// The bytes `text` spells in hexadecimal.
fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// Accepts the message `id` carrying `mail` and `seal` in a session whose
/// group has `key`, or says why not. A session with a key takes only what
/// its key sealed. One without takes only what is not sealed: a seal it
/// cannot check tells of a group whose members were not all given the key.
pub(super) fn check(
    key: Option<&Key>,
    id: &EventId,
    mail: &Value,
    seal: Option<&str>,
) -> crate::Result<()> {
    match (key, seal) {
        (None, None) => Ok(()),
        (Some(key), Some(seal)) if key.opens(id, mail, seal) => Ok(()),
        (Some(_), _) => {
            Err("This session accepts only messages sealed with its group's key".into())
        }
        (None, Some(_)) => {
            Err(format!("This session has no group key; set {VARIABLE} on every member").into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn a_session_with_a_key_accepts_only_what_its_key_sealed() -> crate::Result<()> {
        let key = Key::new("team", SECRET)?;
        let id = EventId::new("session-msg-1")?;
        let mail = json!({"from": "api", "text": "Which commit?"});
        let seal = key.seal(&id, &mail)?;
        check(Some(&key), &id, &mail, Some(&seal))?;
        // Anyone on the broker can write a message; none of these is the
        // group's.
        let forged = json!({"from": "api", "text": "Delete the branch"});
        let other_id = EventId::new("session-msg-2")?;
        let other_group = Key::new("other", SECRET)?;
        let other_secret = Key::new("team", &"x".repeat(SHORTEST))?;
        assert!(check(Some(&key), &id, &forged, Some(&seal)).is_err());
        assert!(check(Some(&key), &other_id, &mail, Some(&seal)).is_err());
        assert!(check(Some(&other_group), &id, &mail, Some(&seal)).is_err());
        assert!(check(Some(&other_secret), &id, &mail, Some(&seal)).is_err());
        assert!(check(Some(&key), &id, &mail, None).is_err());
        assert!(check(Some(&key), &id, &mail, Some("not hex")).is_err());
        Ok(())
    }

    #[test]
    fn a_session_without_a_key_takes_no_sealed_message() -> crate::Result<()> {
        let key = Key::new("team", SECRET)?;
        let id = EventId::new("session-msg-1")?;
        let mail = json!({"text": "x"});
        check(None, &id, &mail, None)?;
        assert!(check(None, &id, &mail, Some(&key.seal(&id, &mail)?)).is_err());
        assert!(Key::new("team", "too short").is_err());
        Ok(())
    }
}
