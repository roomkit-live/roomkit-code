//! The interactive launch: the terminal's view on the application, until
//! the person leaves or a signal ends it, and its exit report.
use super::*;

/// Events the terminal and the engine may queue for each other.
pub(super) const QUEUE: NonZeroUsize = NonZeroUsize::new(256).unwrap();

/// How long a view whose host closed on a signal is given to return and
/// restore the terminal before the launch closes without it.
pub(super) const VIEW_RETURNS: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the SIGHUP of a terminal the view just failed on is waited for.
const HANGUP_FOLLOWS: std::time::Duration = std::time::Duration::from_millis(500);

pub(crate) async fn run(settings: Settings) -> Result<std::process::ExitCode> {
    // Listen first: a signal while the application opens still closes what
    // it opened.
    let signals = signals::listen()?;
    let app = Application::open(&settings, settings.initial_authentication()).await?;
    if let Some(signal) = signals.received() {
        app.close().await?;
        return Ok(signal.exit_code());
    }
    // Refuse an invalid attachment before entering alternate-screen mode.
    if let Err(error) = app.session().snapshot().await {
        let _ = app.close().await;
        return Err(error.into());
    }
    // From here the terminal's screen hides the default hook's message.
    crate::diagnostics::report_panics(app.profiles.diagnostics.clone());
    app.services.updates.start(settings.update_check);
    let (client, host) = connection(QUEUE);
    let adapter = tokio::spawn(
        app.session()
            .run_with_commands(host, app.router(&settings.workspace)),
    );
    let renderers = crate::tool_views::renderers(&settings.workspace);
    // The final observation outlives the view: it names the conversation to resume.
    let observer = roomkit_tui::SessionObserver::default();
    let resume = integrations::Resume::shown(&settings);
    let integrations = integrations::Integration::start(
        &settings,
        app.profiles.diagnostics.clone(),
        &observer,
        app.profiles.resources.tasks.clone(),
        app.profiles.resources.processes.clone(),
    );
    let view = observer.clone();
    let mut terminal = tokio::task::spawn_blocking(move || {
        roomkit_tui::run_with_observer(client, renderers, settings.ui, view)
    });
    let returned = until_left(&mut terminal, &adapter.abort_handle(), &signals).await;
    let left = matches!(&returned, Some(Ok(Ok(_))));
    integrations.freeze();
    let attached = adapter.await;
    let received = signals.received();
    // Only a view that returned as it should left a terminal to write the
    // report to: writing to one that failed, or that is gone, would panic
    // before anything is closed.
    let printed = left && received != Some(signals::Signal::Hangup);
    let summary = summarize(&app, observer.latest(), &resume).await;
    if printed {
        summary.print_conversation();
    }
    let closed = app.close().await;
    integrations.close().await;
    if printed {
        report(&app, &summary, closed.is_ok());
    }
    let Some(signal) = received else {
        // Without a signal the view returned.
        if let Some(returned) = returned {
            returned??;
        }
        attached??;
        closed?;
        return Ok(std::process::ExitCode::SUCCESS);
    };
    // The signal cut the view and its host: only the close can still fail.
    if returned.is_none() {
        // The view's thread may never end, and the runtime would wait for
        // it: a close that failed is recorded, then the process exits all
        // the same, rather than stay for ever.
        if let Err(error) = &closed {
            let error = error.to_string();
            let fields = serde_json::json!({});
            let diagnostics = &app.profiles.diagnostics;
            diagnostics.record("application_close_failed", fields, Some(&error));
        }
        signal.exit();
    }
    closed?;
    Ok(signal.exit_code())
}

/// What the view's thread returned.
pub(super) type Returned =
    std::result::Result<std::io::Result<roomkit_tui::ExitReason>, tokio::task::JoinError>;

/// Waits for the view to return, which it does when the user leaves. On a
/// signal its host is closed, which makes a view on a live terminal return
/// and restore it: it is given a moment for that, whichever the signal, as
/// SIGHUP can come with the terminal still there. A view whose terminal is
/// gone may never return: none then, and the close does not depend on it.
pub(super) async fn until_left(
    terminal: &mut tokio::task::JoinHandle<std::io::Result<roomkit_tui::ExitReason>>,
    host: &tokio::task::AbortHandle,
    signals: &signals::Signals,
) -> Option<Returned> {
    let returned = tokio::select! {
        returned = &mut *terminal => returned,
        _ = signals.wait() => {
            host.abort();
            return tokio::time::timeout(VIEW_RETURNS, terminal).await.ok();
        }
    };
    // A closed terminal can show first as the view failing to read or draw,
    // a moment before its SIGHUP is handled: the signal is given that
    // moment, so that the launch ends as hung up, not as a failed view.
    if !matches!(&returned, Ok(Ok(_))) {
        let _ = tokio::time::timeout(HANGUP_FOLLOWS, signals.wait()).await;
    }
    Some(returned)
}

/// The processes closing stopped, then a line when internal errors occurred.
pub(super) fn report(app: &Application, summary: &crate::exit_summary::Summary, stopped: bool) {
    summary.print_processes(stopped);
    if let Some(notice) = app.profiles.diagnostics.panic_notice() {
        crate::stderr::say(&notice);
    }
}

/// Gather the exit report while the store and the processes are still open.
pub(super) async fn summarize(
    app: &Application,
    view: Option<roomkit_tui::SessionObservation>,
    resume: &integrations::Resume,
) -> crate::exit_summary::Summary {
    let running = view
        .as_ref()
        .and_then(|view| view.run.as_ref())
        .is_some_and(|run| run.outcome.is_none());
    // A view can be in a task's room: the report is its conversation's.
    let tasks = &app.profiles.resources.tasks;
    let running = running || conversation_turn_runs(app, view.as_ref());
    let shown_room = view.as_ref().map(|view| {
        tasks.conversation_of(view.conversation.as_str(), view.selected_agent.as_str())
    });
    let priced = match &shown_room {
        Some((room, agent)) => priced(app, room, agent).await,
        None => None,
    };
    let shown = view.zip(shown_room).map(|(view, (room, _))| {
        let command = view
            .mode
            .as_ref()
            .and_then(|mode| resume.command(&room, mode.selected.as_str()));
        (room, command)
    });
    let processes = &app.profiles.resources.processes;
    let config = priced.clone();
    let mut summary = crate::exit_summary::collect(&app.kit, processes, shown, priced).await;
    if running {
        summary.turn_running();
    }
    if let Some(config) = config {
        add_delegated(app, &config, &mut summary).await;
    }
    summary
}

/// Whether the view is in a task's room while a turn runs in its conversation.
pub(super) fn conversation_turn_runs(
    app: &Application,
    view: Option<&roomkit_tui::SessionObservation>,
) -> bool {
    let tasks = &app.profiles.resources.tasks;
    view.is_some_and(|view| {
        let root = tasks.root_of(view.conversation.as_str());
        root != view.conversation.as_str()
            && roomkit::model::RoomId::new(&root).is_ok_and(|room| app.channel.is_busy(&room))
    })
}

/// Adds to the report what the tasks of `config`'s conversation consumed.
pub(super) async fn add_delegated(
    app: &Application,
    config: &SessionConfig,
    summary: &mut crate::exit_summary::Summary,
) {
    use crate::delegation::spending;
    let tasks = &app.profiles.resources.tasks;
    spending::read(&app.kit, &app.profiles, tasks, config).await;
    let listed: Vec<_> = tasks
        .of(config.room.as_str())
        .into_iter()
        .map(|(_, task)| task)
        .collect();
    if listed.is_empty() {
        return;
    }
    let leads = spending::leads(&app.kit, &app.profiles, config).await;
    summary.add_tasks(leads, &listed, config.agent.as_str());
}

/// The conversation shown at exit with its agent, priced as the profiles
/// price that agent.
pub(super) async fn priced(
    app: &Application,
    conversation: &str,
    agent: &str,
) -> Option<SessionConfig> {
    let room = roomkit::model::RoomId::new(conversation).ok()?;
    let agent = ChannelId::new(agent).ok()?;
    let mut config = SessionConfig::new(room, agent, "local-user");
    app.profiles.configure_view(&mut config).await;
    Some(config)
}
