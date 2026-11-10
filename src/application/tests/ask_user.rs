//! An agent asks the person questions: they reach the view as a card with no
//! approval to give first, in Ask and in Plan, and the model reads the answer,
//! or that the person closed the card.

use roomkit_tui::protocol::{Question, QuestionAnswer};

use super::*;

fn asking() -> Vec<(&'static str, Value)> {
    vec![(
        crate::ask_user::ASK_USER,
        json!({"questions": [{"header": "Database", "question": "Which database?", "options": [
            {"label": "SQLite (Recommended)", "description": "One file"},
            {"label": "Postgres"}
        ]}]}),
    )]
}

/// The question the turn asks, failing on an approval asked first.
async fn asked(client: &roomkit_tui::Client) -> Question {
    loop {
        match event(client).await {
            Event::QuestionAsked { question, .. } => return question,
            Event::PermissionRequested { permission, .. } => {
                panic!("asking needs no approval: {}", permission.title)
            }
            _ => {}
        }
    }
}

async fn finished(client: &roomkit_tui::Client) {
    loop {
        if matches!(event(client).await, Event::OperationFinished { .. }) {
            return;
        }
    }
}

/// What the model was sent back for its question, in the turn's next request.
fn read_back(mock: &Mock) -> String {
    let requests = mock.requests.lock().unwrap();
    // The turn's next request follows the question.
    let next = &requests[1];
    next["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(|message| message["content"].to_string())
        .collect()
}

async fn ask(mode: &str, answers: Option<Vec<QuestionAnswer>>) -> (Mock, Question) {
    let project = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let mock = Mock::start(asking()).await;
    let mut settings = settings(project.path(), state.path(), &mock.base_url);
    if mode == "plan" {
        settings.mode = crate::permissions::Mode::Plan;
    }
    let app = Application::open(&settings, String::new()).await.unwrap();
    let (client, host) = connection(NonZeroUsize::new(256).unwrap());
    let task = tokio::spawn(app.session().run(host));
    event(&client).await;
    let submit = Command::Submit {
        agent: "code".into(),
        text: "Set the store up".into(),
    };
    command(&client, 1, "main", submit);
    let question = asked(&client).await;
    let answer = Command::AnswerQuestion {
        question: question.id.clone(),
        answers,
    };
    command(&client, 2, "main", answer);
    finished(&client).await;
    {
        let requests = mock.requests.lock().unwrap();
        let tools = requests[0]["tools"].to_string();
        assert!(tools.contains("\"ask_user\""), "{tools}");
    }
    drop(client);
    task.await.unwrap().unwrap();
    app.close().await.unwrap();
    (mock, question)
}

#[tokio::test]
async fn the_person_s_answer_reaches_the_model_without_an_approval() {
    let picked = QuestionAnswer::new(vec!["Postgres".to_owned()]);
    let (mock, question) = ask("ask", Some(vec![picked])).await;
    assert_eq!(question.items[0].header, "Database");
    assert_eq!(question.items[0].options[0].label, "SQLite (Recommended)");
    let read = read_back(&mock);
    assert!(read.contains("Database: Postgres"), "{read}");
}

#[tokio::test]
async fn plan_asks_too_and_a_closed_card_tells_the_model_to_go_on() {
    let (mock, _) = ask("plan", None).await;
    let read = read_back(&mock);
    assert!(
        read.contains("closed the question without answering"),
        "{read}"
    );
}
