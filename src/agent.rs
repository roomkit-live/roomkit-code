//! The product's development instructions and selected tools.

use roomkit::model::ChannelId;
use roomkit_ai::{ToolDefinition, Toolset};
use roomkit_openai::{OpenAiAgent, OpenAiConfig};
use roomkit_tools_code::CodeOutlineTool;
use roomkit_tools_fs::{
    DiffTool, EditTool, GlobTool, GrepTool, ListTool, MutationWorkspace, PatchTool, ReadTool,
    WriteTool,
};
use roomkit_tools_process::ShellTool;
use roomkit_tools_roomkit::{
    CodeOutlineToolset, DiffToolset, EditToolset, GlobToolset, GrepToolset, ListToolset,
    PatchToolset, ReadToolset, ShellToolset, WriteToolset,
};

use crate::{
    Result,
    profiles::model::{Agent, Provider},
};

const INSTRUCTIONS: &str = include_str!("coding-agent.txt");

/// Inventory and registration are derived from the same selected toolsets.
macro_rules! assemble {
    ($id:expr, $profile:expr, $provider:expr; $($binding:ident = $tool:expr),+ $(,)?) => {{
        $(let $binding = $tool;)+
        let tools = [$($binding.tools()),+].concat().into_iter().filter(|tool| $profile.tools.contains(&tool.name)).collect::<Vec<_>>();
        let inventory = tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>().join(", ");
        let prompt = format!("{INSTRUCTIONS}\nCurrent agent: {} (ID: {}). Answer as this selected participant. Other participants may have different tools and providers.\nAgent instructions:\n{}\nCurrent capability inventory (provided by the application for this run):\n{inventory}\nOnly tools in this inventory are available to you. Apply tool-specific guidance only when that tool is present.\n", $profile.label, $profile.id, $profile.instructions);
        let mut agent = OpenAiAgent::new($id, $provider.with_system_prompt(prompt))?;
        $(if $binding.tools().iter().any(|tool| $profile.tools.contains(&tool.name)) { agent = agent.with_tools($binding)?; })+
        let agent = agent.with_tool_timeout(Some(std::time::Duration::from_secs(130)));
        Ok((agent, tools))
    }};
}

pub(crate) async fn create(
    root: &std::path::Path,
    profile: &Agent,
    connection: &Provider,
) -> Result<(OpenAiAgent, Vec<ToolDefinition>)> {
    profile.validate()?;
    connection.validate()?;
    let provider = OpenAiConfig::new(
        &connection.base_url,
        connection.authentication.key()?,
        &profile.model,
    )
    .with_timeout(std::time::Duration::from_secs(connection.timeout_seconds));
    let mutations = MutationWorkspace::new(root).await?;
    assemble!(ChannelId::new(&profile.id)?, profile, provider;
        outline = CodeOutlineToolset::new(CodeOutlineTool::new(root).await?),
        list = ListToolset::new(ListTool::new(root).await?),
        glob = GlobToolset::new(GlobTool::new(root).await?),
        grep = GrepToolset::new(GrepTool::new(root).await?),
        read = ReadToolset::new(ReadTool::new(root).await?),
        diff = DiffToolset::new(DiffTool::new(root).await?),
        write = WriteToolset::new(WriteTool::from_workspace(mutations.clone())),
        edit = EditToolset::new(EditTool::from_workspace(mutations.clone())),
        patch = PatchToolset::new(PatchTool::from_workspace(mutations)),
        shell = ShellToolset::new(ShellTool::new(root).await?),
    )
}
