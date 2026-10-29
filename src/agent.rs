//! The product's development instructions and selected tools.

use roomkit::model::ChannelId;
use roomkit_openai::{OpenAiAgent, OpenAiConfig};
use roomkit_tools_fs::{GlobTool, GrepTool, ListTool, ReadTool};
use roomkit_tools_roomkit::{GlobToolset, GrepToolset, ListToolset, ReadToolset};

use crate::{Result, config::Settings};

const INSTRUCTIONS: &str = include_str!("coding-agent.txt");

pub(crate) async fn create(settings: &Settings, api_key: String) -> Result<OpenAiAgent> {
    let provider = OpenAiConfig::new(&settings.base_url, api_key, &settings.model)
        .with_system_prompt(INSTRUCTIONS);
    let root = &settings.workspace;
    Ok(OpenAiAgent::new(ChannelId::new("code")?, provider)?
        .with_tools(ListToolset::new(ListTool::new(root).await?))?
        .with_tools(GlobToolset::new(GlobTool::new(root).await?))?
        .with_tools(GrepToolset::new(GrepTool::new(root).await?))?
        .with_tools(ReadToolset::new(ReadTool::new(root).await?))?)
}
