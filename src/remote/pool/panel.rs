//! The pool directory, its worker and profile details, and conversation placements.
use super::{Scheduler, registry::Row};
use roomkit_tui::protocol::{Choice, Event, PanelSpec};
use roomkit_tui_engine::{CommandResult, SessionConfig};

const COMMAND: &str = "executors";

pub(crate) async fn panel(
    pool: Option<&Scheduler>,
    current: &SessionConfig,
    arguments: &str,
) -> Result<CommandResult, String> {
    let room = current.room.as_str();
    if arguments == "dismiss" {
        return Ok(CommandResult::Events(vec![]));
    }
    let configure = Choice::new("configure", "Configure this workspace");
    let Some(pool) = pool else {
        return Ok(detail(
            room,
            "Executor pool · beta",
            "Scheduling is off for this session.",
            vec![configure],
        ));
    };
    let title = format!("Executor pool · {} · beta", pool.group());
    if arguments == "tasks" {
        return Ok(placements(room, pool));
    }
    let rows = match pool.inventory().rows().await {
        Ok(rows) => rows,
        Err(reason) => {
            return Ok(detail(
                room,
                &title,
                &format!("Inventory unavailable. {reason}."),
                vec![
                    Choice::new("refresh", "Refresh"),
                    configure,
                    Choice::new("tasks", "Task placements"),
                ],
            ));
        }
    };
    if let Some(name) = arguments.strip_prefix("worker:") {
        return Ok(worker_details(room, &rows, name));
    }
    if let Some(name) = arguments.strip_prefix("profiles:") {
        return Ok(profile_picker(room, &rows, name));
    }
    if let Some((name, id)) = arguments
        .strip_prefix("profile:")
        .and_then(|s| s.split_once('/'))
    {
        return Ok(profile_details(room, &rows, name, id));
    }
    let page = arguments
        .strip_prefix("page:")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Ok(directory(room, &title, rows, page))
}

fn placements(room: &str, pool: &Scheduler) -> CommandResult {
    let rows = pool.queue.report(room);
    let body = if rows.is_empty() {
        "No remote tasks in this conversation.".into()
    } else {
        rows.join("\n")
    };
    detail(
        room,
        "Task placements",
        &body,
        vec![Choice::new("tasks", "Refresh"), back()],
    )
}

fn directory(room: &str, title: &str, mut rows: Vec<Row>, page: usize) -> CommandResult {
    // Leave room for navigation in the terminal's 500-choice catalogue.
    const PAGE_SIZE: usize = 495;
    rows.sort_by(|a, b| {
        b.online()
            .cmp(&a.online())
            .then_with(|| a.worker.name.cmp(&b.worker.name))
    });
    let last = rows.len().saturating_sub(1) / PAGE_SIZE;
    let page = page.min(last);
    let mut choices: Vec<_> = rows
        .iter()
        .skip(page * PAGE_SIZE)
        .take(PAGE_SIZE)
        .map(worker_choice)
        .collect();
    if page < last {
        choices.push(Choice::new(format!("page:{}", page + 1), "More executors"));
    }
    if page > 0 {
        choices.push(Choice::new(
            format!("page:{}", page - 1),
            "Previous executors",
        ));
    }
    choices.extend([
        Choice::new("tasks", "Task placements").with_description("This conversation"),
        Choice::new("refresh", "Refresh").with_description("Reload the worker inventory"),
        Choice::new("configure", "Configure this workspace")
            .with_description("Scheduling and worker settings"),
    ]);
    let online = rows.iter().filter(|r| r.online()).count();
    let status = if rows.is_empty() {
        "No workers registered".to_owned()
    } else {
        format!("{online}/{} online", rows.len())
    };
    CommandResult::Events(vec![Event::Choices {
        conversation: room.into(),
        command: COMMAND.into(),
        title: format!("{title} · {status}"),
        choices,
    }])
}

fn back() -> Choice {
    Choice::new("refresh", "All executors")
}

fn detail(room: &str, title: &str, body: &str, actions: Vec<Choice>) -> CommandResult {
    let panel = PanelSpec::new(uuid::Uuid::new_v4().to_string(), COMMAND, title, body)
        .with_actions(actions)
        .with_dismiss("dismiss");
    CommandResult::Events(vec![Event::Panel {
        conversation: room.into(),
        panel,
    }])
}

fn worker_choice(row: &Row) -> Choice {
    let w = &row.worker;
    let status = if row.online() { "Online" } else { "Offline" };
    let free = row.free_slots();
    let mut description = format!("{}/{} · {free}/{} slots free", w.os, w.arch, w.slots);
    for (k, v) in &w.labels {
        description.push_str(&format!(" · {k}={v}"));
    }
    if row.online() {
        if let Some(busy) = w.metrics.busy {
            description.push_str(&format!(" · CPU {busy}%"));
        }
        description.push_str(&format!(
            " · {:.1} GiB free",
            w.metrics.available_mib as f64 / 1024.0
        ));
    }
    let mut title = format!("{} · {status}", w.name);
    if let Some(version) = &w.version {
        title.push_str(&format!(" · v{version}"));
    }
    Choice::new(format!("worker:{}", w.name), title).with_description(description)
}

fn worker_details(room: &str, rows: &[Row], name: &str) -> CommandResult {
    let Some(row) = rows.iter().find(|r| r.worker.name == name) else {
        return detail(
            room,
            "Executor details",
            "This worker is no longer registered.",
            vec![back()],
        );
    };
    let w = &row.worker;
    let status = if row.online() { "Online" } else { "Offline" };
    let mut body = resource_lines(row);
    body.extend([String::new(), "Agent profiles".into()]);
    let mut actions = Vec::new();
    // Keep the worker sheet compact; larger catalogues have their own filter.
    const PREVIEW: usize = 5;
    for (i, p) in w.profiles.iter().take(PREVIEW).enumerate() {
        let default = if i == 0 { " (default)" } else { "" };
        body.push(format!("  {}{default} · {}", p.id, p.model));
        actions.push(Choice::new(
            format!("profile:{name}/{}", p.id),
            format!("Inspect {}", p.id),
        ));
    }
    if w.profiles.len() > PREVIEW {
        body.push(format!("  … and {} more", w.profiles.len() - PREVIEW));
        actions = vec![Choice::new(
            format!("profiles:{name}"),
            format!("Browse {} profiles", w.profiles.len()),
        )];
    }
    if w.profiles.is_empty() {
        body.push(format!(
            "  {}",
            w.model.as_deref().unwrap_or("No profiles reported")
        ));
    }
    actions.push(back());
    detail(
        room,
        &format!("{name} · {status}"),
        &body.join("\n"),
        actions,
    )
}

fn resource_lines(row: &Row) -> Vec<String> {
    let w = &row.worker;
    let m = &w.metrics;
    let labels = w
        .labels
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let host = w.host_os.as_deref().unwrap_or("unknown");
    let cpu = m
        .busy
        .map_or_else(|| "sampling".into(), |n| format!("{n}% busy"));
    let load = m
        .load
        .map_or_else(|| "unknown".into(), |n| format!("{n:.2}"));
    let mut body = vec![
        format!("Workspace  {}", w.workspace_display()),
        format!(
            "Version    {}",
            w.version.as_deref().unwrap_or("Not reported")
        ),
        format!("System     {}/{} · host {host}", w.os, w.arch),
        format!("Capacity   {}/{} slots free", row.free_slots(), w.slots),
        format!("CPU        {} cores · {cpu} · load {load}", m.cpus),
        format!(
            "Memory     {:.1} GiB available",
            m.available_mib as f64 / 1024.0
        ),
    ];
    if !labels.is_empty() {
        body.push(format!("Labels     {labels}"));
    }
    body
}

fn profile_picker(room: &str, rows: &[Row], name: &str) -> CommandResult {
    let Some(row) = rows.iter().find(|r| r.worker.name == name) else {
        return worker_details(room, rows, name);
    };
    let mut choices: Vec<_> = row
        .worker
        .profiles
        .iter()
        .enumerate()
        .map(|(i, profile)| {
            let label = if i == 0 {
                format!("{} (default)", profile.id)
            } else {
                profile.id.clone()
            };
            Choice::new(format!("profile:{name}/{}", profile.id), label)
                .with_description(profile.model.clone())
        })
        .collect();
    choices.push(Choice::new(
        format!("worker:{name}"),
        format!("Back to {name}"),
    ));
    CommandResult::Events(vec![Event::Choices {
        conversation: room.into(),
        command: COMMAND.into(),
        title: format!("{name} · Agent profiles"),
        choices,
    }])
}

fn profile_details(room: &str, rows: &[Row], name: &str, id: &str) -> CommandResult {
    let profile = rows
        .iter()
        .find(|r| r.worker.name == name)
        .and_then(|r| r.worker.profiles.iter().find(|p| p.id == id));
    let Some(p) = profile else {
        return detail(
            room,
            "Profile details",
            "This profile is no longer advertised.",
            vec![back()],
        );
    };
    let mut body = vec![
        p.label.clone(),
        format!("Model   {}", p.model),
        String::new(),
        format!("Tools ({})", p.tools.len()),
    ];
    body.extend(p.tools.iter().map(|t| format!("  {t}")));
    detail(
        room,
        &format!("{name} · {id}"),
        &body.join("\n"),
        vec![
            Choice::new(format!("worker:{name}"), format!("Back to {name}")),
            back(),
        ],
    )
}

#[cfg(test)]
mod tests;
