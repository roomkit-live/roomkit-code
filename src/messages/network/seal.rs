//! The session group's key: what proves that a message comes from a member
//! of the group, not from anyone who can write to its broker.
//!
//! The broker carries every message, so it cannot hold what authenticates
//! them: the key is given to each member outside it, in
//! `ROOMKIT_SESSION_GROUP_KEY`. A member that has one seals what it sends
//! and accepts only what is sealed with it, and its mailbox answers a
//! message it took with a receipt sealed the same way: a sender is not told
//! "delivered" by whoever answers in the broker in a member's place. The
//! members of a group trust each other: the key tells them from outsiders,
//! not from one another.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use hmac::{Hmac, KeyInit, Mac};
use roomkit::model::EventId;
use serde_json::{Value, json};
use sha2::Sha256;

use crate::diagnostics::Diagnostics;

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
            return Err(format!("{VARIABLE} must hold at least {SHORTEST} bytes").into());
        }
        Ok(Self {
            secret: secret.as_bytes().to_vec(),
            group: group.to_owned(),
        })
    }

    /// The authenticator of `what`, in this key's group. `what` starts
    /// with what it seals, a message or a receipt, so that one is never
    /// taken for the other.
    fn mac(&self, what: &[&Value]) -> Option<Hmac<Sha256>> {
        let mut sealed = vec![json!(self.group)];
        sealed.extend(what.iter().map(|value| (*value).clone()));
        let sealed = serde_json::to_vec(&sealed).ok()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.secret).ok()?;
        mac.update(&sealed);
        Some(mac)
    }

    /// What seals the message `id` carrying `mail`: the mail names its
    /// sender and its recipient, so a sealed message cannot be given
    /// another of either.
    fn message(&self, id: &EventId, mail: &Value) -> Option<Hmac<Sha256>> {
        self.mac(&[&json!("message"), &json!(id), mail])
    }

    /// What seals the receipt of the message `id`.
    fn receipt_of(&self, id: &EventId) -> Option<Hmac<Sha256>> {
        self.mac(&[&json!("receipt"), &json!(id)])
    }

    /// The seal a message leaves with.
    fn seal(&self, id: &EventId, mail: &Value) -> crate::Result<String> {
        let mac = self.message(id, mail).ok_or("Cannot seal the message")?;
        Ok(crate::digest::hex(&mac.finalize().into_bytes()))
    }

    /// Whether `seal` is the one this key gives the message, compared
    /// without telling how much of it matched.
    fn opens(&self, id: &EventId, mail: &Value, seal: &str) -> bool {
        matches(self.message(id, mail), seal)
    }
}

/// Whether `mac` gives `seal`, compared without telling how much matched.
fn matches(mac: Option<Hmac<Sha256>>, seal: &str) -> bool {
    let Some(given) = unhex(seal) else {
        return false;
    };
    mac.is_some_and(|mac| mac.verify_slice(&given).is_ok())
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

/// Why a session refused a message for its seal: a closed list both ends
/// of a group know, so that the sender can say what to fix without ever
/// quoting what a peer answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The refusing session has a key; the message has no seal.
    Unsealed,
    /// The refusing session's key does not open the message's seal.
    OtherKey,
    /// The refusing session has no key; the message is sealed.
    Keyless,
}

impl Refusal {
    const ALL: [Self; 3] = [Self::Unsealed, Self::OtherKey, Self::Keyless];

    /// What the diagnostics record.
    fn code(self) -> &'static str {
        match self {
            Self::Unsealed => "unsealed",
            Self::OtherKey => "other_key",
            Self::Keyless => "keyless",
        }
    }

    /// What the refusing session answers.
    fn answer(self) -> &'static str {
        match self {
            Self::Unsealed => {
                "This session accepts only messages sealed with its group's key; this one is not sealed"
            }
            Self::OtherKey => "This session's group key does not open this message's seal",
            Self::Keyless => "This session has no group key; this message is sealed",
        }
    }

    /// The refusal `answer` is, when a peer answered one.
    pub(super) fn answered(answer: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|refusal| refusal.answer() == answer)
    }

    /// What the sending session is told.
    pub(super) fn told(self) -> String {
        match self {
            Self::Unsealed => format!(
                "Remote session refused the message: it accepts only messages sealed with its group's key, and this session has no {VARIABLE}"
            ),
            Self::OtherKey => format!(
                "Remote session refused the message: its {VARIABLE} differs from this session's"
            ),
            Self::Keyless => format!(
                "Remote session refused the message: it has no {VARIABLE}, and this session seals what it sends"
            ),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.answer())
    }
}

impl std::error::Error for Refusal {}

/// Accepts the message `id` carrying `mail` and `seal` in a session whose
/// group has `key`, or says why not. A session with a key takes only what
/// its key sealed. One without takes only what is not sealed: a seal it
/// cannot check tells of a group whose members were not all given the key.
fn check(key: Option<&Key>, id: &EventId, mail: &Value, seal: Option<&str>) -> Result<(), Refusal> {
    match (key, seal) {
        (None, None) => Ok(()),
        (Some(key), Some(seal)) if key.opens(id, mail, seal) => Ok(()),
        (Some(_), None) => Err(Refusal::Unsealed),
        (Some(_), Some(_)) => Err(Refusal::OtherKey),
        (None, Some(_)) => Err(Refusal::Keyless),
    }
}

/// What a session checks the group's messages and receipts with: its key,
/// if it has one, and the log a refusal is recorded in.
#[derive(Clone)]
pub(in crate::messages) struct Guard {
    key: Option<Key>,
    diagnostics: Arc<Diagnostics>,
    /// The refusals already recorded, one bit each: a broker flooded with
    /// unsealed messages adds a line per kind to the log, not one each.
    recorded: Arc<AtomicU8>,
}

impl Guard {
    pub(in crate::messages) fn new(key: Option<Key>, diagnostics: Arc<Diagnostics>) -> Self {
        Self {
            key,
            diagnostics,
            recorded: Arc::default(),
        }
    }

    /// The seal of a message this session sends, if it has a key.
    pub(super) fn seal(&self, id: &EventId, mail: &Value) -> crate::Result<Option<String>> {
        self.key.as_ref().map(|key| key.seal(id, mail)).transpose()
    }

    /// Check a directory query or reply without reporting a mail delivery failure.
    pub(super) fn authentic(&self, id: &EventId, payload: &Value, seal: Option<&str>) -> bool {
        check(self.key.as_ref(), id, payload, seal).is_ok()
    }

    /// Accepts the message `id` carrying `mail` and `seal`, or records the
    /// refusal, the first of its kind, and returns it.
    pub(super) fn admit(
        &self,
        id: &EventId,
        mail: &Value,
        seal: Option<&str>,
    ) -> Result<(), Refusal> {
        let checked = check(self.key.as_ref(), id, mail, seal);
        if let Err(refusal) = checked {
            let bit = 1 << refusal as u8;
            if self.recorded.fetch_or(bit, Ordering::Relaxed) & bit == 0 {
                let fields = json!({"refusal": refusal.code()});
                self.diagnostics
                    .record("session_message_refused", fields, None);
            }
        }
        checked
    }

    /// What this session's mailbox answers once it took the message `id`.
    pub(super) fn receipt(&self, id: &EventId) -> Value {
        let seal = self.key.as_ref().and_then(|key| key.receipt_of(id));
        match seal {
            Some(seal) => {
                json!({"id": id, "receipt": crate::digest::hex(&seal.finalize().into_bytes())})
            }
            None => json!(id),
        }
    }

    /// Whether `answer` is the receipt of the message `id` a member of the
    /// group gives: sealed with the key, when this session has one.
    pub(super) fn confirms(&self, id: &EventId, answer: &Value) -> bool {
        let Some(key) = &self.key else {
            return *answer == json!(id);
        };
        let receipt = answer.get("receipt").and_then(Value::as_str);
        answer.get("id") == Some(&json!(id))
            && receipt.is_some_and(|receipt| matches(key.receipt_of(id), receipt))
    }

    /// Whether this session seals what it sends.
    pub(in crate::messages) fn sealed(&self) -> bool {
        self.key.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn guard(key: Option<Key>) -> crate::Result<(Guard, tempfile::TempDir)> {
        let dir = tempfile::tempdir()?;
        let log = Diagnostics::open(dir.path().join("diagnostics.jsonl"), false)?;
        Ok((Guard::new(key, log), dir))
    }

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
        let refused = |key: &Key, id: &EventId, mail: &Value, seal: Option<&str>| {
            check(Some(key), id, mail, seal).err()
        };
        let other = Some(Refusal::OtherKey);
        assert_eq!(refused(&key, &id, &forged, Some(&seal)), other);
        assert_eq!(refused(&key, &other_id, &mail, Some(&seal)), other);
        assert_eq!(refused(&other_group, &id, &mail, Some(&seal)), other);
        assert_eq!(refused(&other_secret, &id, &mail, Some(&seal)), other);
        assert_eq!(refused(&key, &id, &mail, Some("not hex")), other);
        assert_eq!(refused(&key, &id, &mail, None), Some(Refusal::Unsealed));
        Ok(())
    }

    #[test]
    fn a_session_without_a_key_takes_no_sealed_message() -> crate::Result<()> {
        let key = Key::new("team", SECRET)?;
        let id = EventId::new("session-msg-1")?;
        let mail = json!({"text": "x"});
        check(None, &id, &mail, None)?;
        let sealed = check(None, &id, &mail, Some(&key.seal(&id, &mail)?));
        assert_eq!(sealed, Err(Refusal::Keyless));
        assert!(Key::new("team", "too short").is_err());
        Ok(())
    }

    /// Review finding: a sender took any answer equal to the message's ID
    /// for its delivery, and anyone who writes to the broker can answer
    /// in a member's place.
    #[test]
    fn a_receipt_counts_only_when_the_group_s_key_sealed_it() -> crate::Result<()> {
        let id = EventId::new("session-msg-1")?;
        let other_id = EventId::new("session-msg-2")?;
        let (member, _log) = guard(Some(Key::new("team", SECRET)?))?;
        let (outsider, _other_log) = guard(Some(Key::new("team", &"x".repeat(SHORTEST))?))?;
        let (open, _open_log) = guard(None)?;
        assert!(member.confirms(&id, &member.receipt(&id)));
        assert!(!member.confirms(&id, &json!(id)));
        assert!(!member.confirms(&id, &outsider.receipt(&id)));
        assert!(!member.confirms(&id, &member.receipt(&other_id)));
        // A message's seal is no receipt.
        let mail = json!({"text": "x"});
        let seal = member.seal(&id, &mail)?;
        assert!(!member.confirms(&id, &json!({"id": id, "receipt": seal})));
        // A group without a key confirms as before.
        assert!(open.confirms(&id, &open.receipt(&id)));
        assert!(!open.confirms(&id, &json!(other_id)));
        Ok(())
    }

    /// Review finding: a refused message left no trace in the refusing
    /// session, and its sender read the same sentence whatever the cause.
    #[test]
    fn a_refusal_is_recorded_once_per_kind_and_told_to_its_sender() -> crate::Result<()> {
        for refusal in Refusal::ALL {
            assert_eq!(Refusal::answered(&refusal.to_string()), Some(refusal));
            assert!(refusal.told().contains(VARIABLE));
        }
        assert_eq!(Refusal::answered("Anything a peer writes"), None);
        let (member, log) = guard(Some(Key::new("team", SECRET)?))?;
        let id = EventId::new("session-msg-1")?;
        let mail = json!({"text": "x"});
        for _ in 0..3 {
            assert_eq!(member.admit(&id, &mail, None), Err(Refusal::Unsealed));
        }
        assert_eq!(member.admit(&id, &mail, Some("00")), Err(Refusal::OtherKey));
        let written = std::fs::read_to_string(log.path().join("diagnostics.jsonl"))?;
        let refusals: Vec<&str> = written
            .lines()
            .filter(|line| line.contains("session_message_refused"))
            .collect();
        assert_eq!(refusals.len(), 2, "{written}");
        assert!(
            refusals[0].contains("\"refusal\":\"unsealed\""),
            "{written}"
        );
        assert!(
            refusals[1].contains("\"refusal\":\"other_key\""),
            "{written}"
        );
        Ok(())
    }
}
