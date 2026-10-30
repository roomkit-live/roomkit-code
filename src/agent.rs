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

use crate::{Result, config::Settings};

const INSTRUCTIONS: &str = include_str!("coding-agent.txt");

/// Inventory and registration are derived from the same selected toolsets.
macro_rules! assemble {
    ($provider:expr; $($binding:ident = $tool:expr),+ $(,)?) => {{
        $(let $binding = $tool;)+
        let tools = [$($binding.tools()),+].concat();
        let inventory = tools.iter().map(|tool| tool.name.as_str()).collect::<Vec<_>>().join(", ");
        let prompt = format!("{INSTRUCTIONS}\nCurrent capability inventory (provided by the application for this run):\n{inventory}\n");
        let agent = OpenAiAgent::new(ChannelId::new("code")?, $provider.with_system_prompt(prompt))?
            $(.with_tools($binding)?)+
            .with_tool_timeout(Some(std::time::Duration::from_secs(130)));
        Ok((agent, tools))
    }};
}

pub(crate) async fn create(
    settings: &Settings,
    api_key: String,
) -> Result<(OpenAiAgent, Vec<ToolDefinition>)> {
    let provider = OpenAiConfig::new(&settings.base_url, api_key, &settings.model).with_timeout(
        std::time::Duration::from_secs(settings.provider_timeout_seconds),
    );
    let root = &settings.workspace;
    let mutations = MutationWorkspace::new(root).await?;
    assemble!(provider;
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
