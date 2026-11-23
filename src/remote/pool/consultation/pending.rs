//! A bounded wait belongs to two specific turns. Dropping the request withdraws it.
use crate::sync::lock;
use std::{collections::HashMap, sync::Mutex};
use tokio::sync::oneshot;

struct Pending {
    from: String,
    to: String,
    reply: Option<oneshot::Sender<String>>,
}

#[derive(Default)]
pub(in crate::remote::pool) struct Consultations(Mutex<HashMap<String, Pending>>);

pub(super) struct Waiting<'a> {
    questions: &'a Consultations,
    pub id: String,
    pub reply: oneshot::Receiver<String>,
}

impl Consultations {
    pub(super) fn open(&self, from: &str, to: &str) -> Result<Waiting<'_>, String> {
        let mut pending = lock(&self.0);
        if from == to
            || pending
                .values()
                .any(|q| [from, to].iter().any(|id| *id == q.from || *id == q.to))
        {
            return Err(
                "A consultation requires two distinct turns that are free to answer".into(),
            );
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, reply) = oneshot::channel();
        pending.insert(
            id.clone(),
            Pending {
                from: from.into(),
                to: to.into(),
                reply: Some(tx),
            },
        );
        Ok(Waiting {
            questions: self,
            id,
            reply,
        })
    }

    pub(super) fn answer(&self, turn: &str, id: &str, answer: String) -> Result<(), String> {
        let sender = lock(&self.0)
            .get_mut(id)
            .filter(|q| q.to == turn)
            .and_then(|q| q.reply.take());
        sender
            .ok_or("No open question with that ID is addressed to this turn")?
            .send(answer)
            .map_err(|_| "The asking turn has stopped waiting".into())
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        lock(&self.questions.0).remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn consultation_is_addressed_once_and_releases_both_turns() -> crate::Result<()> {
        let questions = Consultations::default();
        assert!(questions.open("a", "a").is_err());
        let mut first = questions.open("a", "b")?;
        assert!(questions.open("b", "a").is_err());
        assert!(questions.open("c", "b").is_err());
        assert!(questions.answer("c", &first.id, "other".into()).is_err());
        questions.answer("b", &first.id, "result".into())?;
        assert!(questions.answer("b", &first.id, "again".into()).is_err());
        assert_eq!((&mut first.reply).await?, "result");
        drop(first);
        let abandoned = questions.open("b", "a")?;
        let id = abandoned.id.clone();
        drop(abandoned);
        assert!(questions.answer("a", &id, "late".into()).is_err());
        assert!(questions.open("b", "a").is_ok());
        Ok(())
    }
}
