//! The product's development instructions and selected tools.

use roomkit::model::ChannelId;
use roomkit_openai::{OpenAiAgent, OpenAiConfig};
use roomkit_tools_fs::{
    DiffTool, EditTool, GlobTool, GrepTool, ListTool, MutationWorkspace, PatchTool, ReadTool,
    WriteTool,
};
use roomkit_tools_process::ShellTool;
use roomkit_tools_roomkit::{
    DiffToolset, EditToolset, GlobToolset, GrepToolset, ListToolset, PatchToolset, ReadToolset,
    ShellToolset, WriteToolset,
};

use crate::{Result, config::Settings};

const INSTRUCTIONS: &str = include_str!("coding-agent.txt");

pub(crate) async fn create(settings: &Settings, api_key: String) -> Result<OpenAiAgent> {
    let provider = OpenAiConfig::new(&settings.base_url, api_key, &settings.model)
        .with_system_prompt(INSTRUCTIONS);
    let root = &settings.workspace;
    let mutations = MutationWorkspace::new(root).await?;
    Ok(OpenAiAgent::new(ChannelId::new("code")?, provider)?
        .with_tools(ListToolset::new(ListTool::new(root).await?))?
        .with_tools(GlobToolset::new(GlobTool::new(root).await?))?
        .with_tools(GrepToolset::new(GrepTool::new(root).await?))?
        .with_tools(ReadToolset::new(ReadTool::new(root).await?))?
        .with_tools(DiffToolset::new(DiffTool::new(root).await?))?
        .with_tools(WriteToolset::new(WriteTool::from_workspace(
            mutations.clone(),
        )))?
        .with_tools(EditToolset::new(EditTool::from_workspace(
            mutations.clone(),
        )))?
        .with_tools(PatchToolset::new(PatchTool::from_workspace(mutations)))?
        .with_tools(ShellToolset::new(ShellTool::new(root).await?))?
        .with_tool_timeout(Some(std::time::Duration::from_secs(130))))
}
