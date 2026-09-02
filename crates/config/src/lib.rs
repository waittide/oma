use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use oma_contract::{AgentSummary, ApprovalMode};
use oma_mcp::McpServerConfig;
use oma_provider::{ModelConfig, ProviderConfig};
use serde::{Deserialize, Serialize};

/// 默认监听地址
pub const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:17431";

/// 服务端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_listen_addr")]
    pub listen_addr: String,
}

fn default_listen_addr() -> String {
    DEFAULT_LISTEN_ADDR.to_string()
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
        }
    }
}

/// Oma 根配置文件 (~/.config/oma/config.toml)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OmaConfig {
    #[serde(default = "default_model_str")]
    pub default_model: String,
    #[serde(default = "default_agent_str")]
    pub default_agent: String,
    #[serde(default)]
    pub default_approval_mode: ApprovalMode,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
}

fn default_model_str() -> String {
    "my_anthropic/claude-3-7-sonnet".to_string()
}
fn default_agent_str() -> String {
    "task".to_string()
}

impl Default for OmaConfig {
    fn default() -> Self {
        Self {
            default_model: default_model_str(),
            default_agent: default_agent_str(),
            default_approval_mode: ApprovalMode::Normal,
            server: ServerConfig::default(),
            providers: BTreeMap::new(),
            mcp_servers: BTreeMap::new(),
        }
    }
}

impl OmaConfig {
    /// 从文件加载配置
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: OmaConfig = toml::from_str(&content)?;
        Ok(config)
    }

    /// 自动从标准路径加载配置（若不存在则返回默认配置）
    pub fn load_or_default() -> Self {
        if let Some(config_dir) = dirs_config_dir() {
            let path = config_dir.join("oma").join("config.toml");
            if path.exists() {
                if let Ok(cfg) = Self::load_from_file(&path) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    /// 根据 "provider/model" 字符串定位 Provider 与 Model 配置
    pub fn find_model(&self, selector: &str) -> Option<(&ProviderConfig, ModelConfig)> {
        let mut parts = selector.splitn(2, '/');
        let provider_name = parts.next()?;
        let model_id = parts.next().unwrap_or(provider_name);

        let provider = self.providers.get(provider_name)?;

        // 尝试从 provider.body 或通用字段寻找模型，若未配置具体 model 列表，则构造默认 ModelConfig
        Some((
            provider,
            ModelConfig {
                id: model_id.to_string(),
                name: model_id.to_string(),
                context_len: 128_000,
                supports_vision: true,
                supports_thinking: true,
                headers: BTreeMap::new(),
                body: serde_json::json!({}),
            },
        ))
    }
}

pub fn dirs_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

// =========================================================================
// Agent 模板体系与动态环境上下文拼装
// =========================================================================

/// 内置 Agent 模板源码
pub const TEMPLATE_TASK: &str = include_str!("templates/task.md");
pub const TEMPLATE_PLAN: &str = include_str!("templates/plan.md");
pub const TEMPLATE_EXPLORE: &str = include_str!("templates/explore.md");
pub const TEMPLATE_REVIEW: &str = include_str!("templates/review.md");
pub const TEMPLATE_BUILD: &str = include_str!("templates/build.md");

/// 解析后的 Agent 模板定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tools: Vec<String>,
    pub system_prompt_body: String,
}

/// Agent Frontmatter 头部结构
#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools: Vec<String>,
}

/// 解析 Markdown 的 Frontmatter 与正文
pub fn parse_markdown_template(id: &str, raw: &str) -> Result<AgentTemplate> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("---") {
        if let Some(end_pos) = rest.find("---") {
            let yaml_str = &rest[..end_pos];
            let body = rest[end_pos + 3..].trim();

            let fm: Frontmatter = serde_yaml::from_str(yaml_str)
                .with_context(|| format!("Failed to parse YAML frontmatter for agent {}", id))?;

            return Ok(AgentTemplate {
                id: id.to_string(),
                name: fm.name,
                description: fm.description,
                tools: fm.tools,
                system_prompt_body: body.to_string(),
            });
        }
    }

    Ok(AgentTemplate {
        id: id.to_string(),
        name: id.to_string(),
        description: format!("Agent {}", id),
        tools: Vec::new(),
        system_prompt_body: raw.to_string(),
    })
}

/// Agent 模板加载器
pub struct AgentLoader;

impl AgentLoader {
    /// 加载指定 Agent 模板（项目目录 > 全局用户目录 > 二进制内嵌）
    pub fn load_agent(agent_id: &str, workspace: &Path) -> Result<AgentTemplate> {
        // 1. 项目级覆盖: <workspace>/.oma/agents/<agent_id>.md
        let project_path = workspace.join(".oma").join("agents").join(format!("{}.md", agent_id));
        if project_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&project_path) {
                return parse_markdown_template(agent_id, &raw);
            }
        }

        // 2. 用户全局配置: ~/.config/oma/agents/<agent_id>.md
        if let Some(config_dir) = dirs_config_dir() {
            let user_path = config_dir.join("oma").join("agents").join(format!("{}.md", agent_id));
            if user_path.exists() {
                if let Ok(raw) = std::fs::read_to_string(&user_path) {
                    return parse_markdown_template(agent_id, &raw);
                }
            }
        }

        // 3. 编译期内嵌兜底
        let bundled = match agent_id {
            "task" => TEMPLATE_TASK,
            "plan" => TEMPLATE_PLAN,
            "explore" => TEMPLATE_EXPLORE,
            "review" => TEMPLATE_REVIEW,
            "build" => TEMPLATE_BUILD,
            _ => anyhow::bail!("Agent template '{}' not found", agent_id),
        };

        parse_markdown_template(agent_id, bundled)
    }

    /// 列出所有可用的 Agent 元数据列表
    pub fn list_agents(workspace: &Path) -> Vec<AgentSummary> {
        let default_ids = ["task", "plan", "explore", "review", "build"];
        let mut list = Vec::new();

        for id in default_ids {
            if let Ok(tmpl) = Self::load_agent(id, workspace) {
                list.push(AgentSummary {
                    id: tmpl.id,
                    name: tmpl.name,
                    description: tmpl.description,
                });
            }
        }

        list
    }

    /// 动态拼装注入实时环境块的最终 System Prompt
    pub fn build_system_prompt(
        template: &AgentTemplate,
        workspace: &Path,
        active_model: &str,
    ) -> String {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        format!(
            "{}\n\n<runtime_context>\n- Workspace: {}\n- Operating System: {} ({})\n- Today: {}\n- Active Model: {}\n</runtime_context>",
            template.system_prompt_body,
            workspace.display(),
            os,
            arch,
            today,
            active_model
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_bundled_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();

        let task_agent = AgentLoader::load_agent("task", ws).unwrap();
        assert_eq!(task_agent.id, "task");
        assert_eq!(task_agent.name, "Task");
        assert!(task_agent.tools.contains(&"read".to_string()));
        assert!(task_agent.tools.contains(&"write".to_string()));
        assert!(task_agent.tools.contains(&"edit".to_string()));
        assert!(task_agent.tools.contains(&"shell".to_string()));
        assert!(task_agent.tools.contains(&"task".to_string()));

        let prompt = AgentLoader::build_system_prompt(&task_agent, ws, "my_anthropic/claude-3-7");
        assert!(prompt.contains("<runtime_context>"));
        assert!(prompt.contains("Active Model: my_anthropic/claude-3-7"));
    }

    #[test]
    fn test_parse_toml_config() {
        let toml_str = r#"
default_model = "deepseek/deepseek-chat"
default_agent = "task"
default_approval_mode = "strict"

[server]
listen_addr = "0.0.0.0:17431"

[providers.deepseek]
api_type = "completion"
base_url = "https://api.deepseek.com/v1"
api_key = "env:DEEPSEEK_KEY"
"#;
        let config: OmaConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.default_model, "deepseek/deepseek-chat");
        assert_eq!(config.server.listen_addr, "0.0.0.0:17431");
        assert!(config.providers.contains_key("deepseek"));
    }
}
