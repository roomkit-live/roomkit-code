//! Application information for the generic terminal sidebar. No credentials or
//! endpoint configuration cross this presentation boundary.
use super::*;
use crate::agent::Composition;
use roomkit_tui::protocol::{
    MeterColor, MeterSegment, SidebarAction, SidebarContent, SidebarEntry, SidebarSection,
    SidebarSpec, SidebarTone,
};

pub(crate) fn action(command: &str, arguments: impl Into<String>) -> SidebarAction {
    SidebarAction::new(command, arguments)
}

/// The terminal refuses a sidebar whose breakdown has more parts than this.
const MAX_PARTS: usize = 16;
/// Fixed parts take the first color slots; servers cycle through the rest.
const SERVER_SLOTS: std::ops::Range<u8> = 4..8;

/// The context gauge split into what every request carries before its
/// messages. Sizes are estimated at four characters per token; the terminal
/// adds the measured remainder as `Messages`. Servers beyond the part limit
/// are summed, smallest first, into one part.
pub(super) fn context(composition: Option<&Composition>, instructions: usize) -> SidebarContent {
    let Some(composition) = composition else {
        return SidebarContent::Context;
    };
    let fixed = [
        ("System prompt", composition.system),
        ("Instructions", instructions),
        ("Tools", composition.tools),
        ("Skills", composition.skills),
    ];
    let mut parts: Vec<_> = fixed
        .into_iter()
        .zip(0..)
        .filter(|((_, chars), _)| *chars > 0)
        .map(|((label, chars), slot)| part(label.to_owned(), chars, MeterColor::Series(slot)))
        .collect();
    let mut servers: Vec<_> = composition
        .mcp
        .iter()
        .filter(|(_, chars)| *chars > 0)
        .collect();
    let room = MAX_PARTS - parts.len();
    let mut others = Vec::new();
    if servers.len() > room {
        let mut by_size = servers.clone();
        by_size.sort_by_key(|(_, chars)| *chars);
        others = by_size[..=servers.len() - room].to_vec();
        servers.retain(|server| !others.contains(server));
    }
    let slots = SERVER_SLOTS.cycle();
    parts.extend(
        servers
            .into_iter()
            .zip(slots)
            .map(|((label, chars), slot)| {
                part(format!("MCP · {label}"), *chars, MeterColor::Series(slot))
            }),
    );
    if !others.is_empty() {
        let chars = others.iter().map(|(_, chars)| chars).sum();
        let label = format!("MCP · {} others", others.len());
        parts.push(part(label, chars, MeterColor::Muted));
    }
    SidebarContent::ContextBreakdown {
        parts,
        remainder: "Messages".into(),
    }
}

fn part(label: String, chars: usize, color: MeterColor) -> MeterSegment {
    let tokens = u64::try_from(chars.div_ceil(4)).unwrap_or(u64::MAX);
    MeterSegment::new(label, tokens, color).estimated()
}

/// The line the engine puts before an application instruction: the length of
/// roomkit-core's `transcript::marks::INSTRUCTION_MARK`, which is not public.
const INSTRUCTION_MARK: usize = 126 + 1;

impl Profiles {
    /// The mode instruction and project instructions each turn adds, as the
    /// engine delivers them.
    async fn instruction_chars(&self, current: &SessionConfig) -> usize {
        let project = self.resources.instructions.load(vec![], true).await;
        let mode = self.policy.instruction(current.room.as_str());
        INSTRUCTION_MARK + mode.len() + 2 + project.render().len()
    }

    pub(crate) async fn sidebar(&self, current: &SessionConfig) -> (SidebarSpec, bool) {
        let instructions = self.instruction_chars(current).await;
        let state = self.state.lock().await;
        let composition = state.compositions.get(current.agent.as_str());
        let agent = state.agent(current.agent.as_str());
        let mut agent_entries = Vec::new();
        if let Some(profile) = agent {
            let label = state.provider_label(&profile);
            agent_entries.push(
                SidebarEntry::new(label.chars().take(64).collect::<String>())
                    .with_tone(SidebarTone::Muted)
                    .with_action(action("providers", "")),
            );
        }
        let mcp = mcp::sidebar(&state, current, &self.mcp_runtime).await;
        let configured = !state.mcp.value.servers.is_empty();
        (
            SidebarSpec::new(
                "Session",
                vec![
                    SidebarSection::new("Agent / model", SidebarContent::Agent)
                        .with_action(action("agents", "")),
                    SidebarSection::new(
                        "Provider",
                        SidebarContent::Entries {
                            entries: agent_entries,
                        },
                    ),
                    SidebarSection::new("Context", context(composition, instructions)),
                    self.plans.sidebar(current).await,
                    self.resources.files.sidebar(current.room.as_str()).await,
                    mcp,
                    self.processes.sidebar(),
                ],
            ),
            configured,
        )
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::panic, reason = "test assertions")]
    use super::*;

    #[test]
    fn the_context_lists_each_fixed_part_with_a_stable_color() {
        let composition = Composition {
            system: 400,
            tools: 802,
            skills: 0,
            mcp: vec![("calc".into(), 4000)],
        };
        let SidebarContent::ContextBreakdown { parts, remainder } = context(Some(&composition), 40)
        else {
            panic!("Expected a breakdown");
        };
        assert_eq!(remainder, "Messages");
        let parts: Vec<_> = parts
            .iter()
            .map(|p| (p.label.as_str(), p.value, p.color, p.estimated))
            .collect();
        // An empty part is left out; the others keep their color.
        assert_eq!(
            parts,
            [
                ("System prompt", 100, MeterColor::Series(0), true),
                ("Instructions", 10, MeterColor::Series(1), true),
                ("Tools", 201, MeterColor::Series(2), true),
                ("MCP · calc", 1000, MeterColor::Series(4), true),
            ]
        );
        // An agent not yet configured has nothing to break down.
        assert_eq!(context(None, 40), SidebarContent::Context);
    }

    /// Review finding: 13 servers made 17 parts, and the terminal then refused
    /// the whole sidebar; the fifth server also took the system prompt's color.
    #[test]
    fn many_servers_fold_into_one_part_within_the_terminal_limit() {
        let composition = Composition {
            system: 400,
            tools: 400,
            skills: 400,
            mcp: (1..=14).map(|n| (format!("s{n}"), n * 40)).collect(),
        };
        let SidebarContent::ContextBreakdown { parts, .. } = context(Some(&composition), 400)
        else {
            panic!("Expected a breakdown");
        };
        assert_eq!(parts.len(), MAX_PARTS);
        let servers: Vec<_> = parts[4..]
            .iter()
            .map(|p| (p.label.as_str(), p.color))
            .collect();
        // The three smallest are summed; the others keep their order and cycle
        // through the server colors, never a fixed part's.
        assert_eq!(servers[0], ("MCP · s4", MeterColor::Series(4)));
        assert_eq!(servers[4], ("MCP · s8", MeterColor::Series(4)));
        assert_eq!(servers[10], ("MCP · s14", MeterColor::Series(6)));
        assert_eq!(servers[11], ("MCP · 3 others", MeterColor::Muted));
        assert_eq!(parts[15].value, (40 + 80 + 120) / 4);
    }
}
