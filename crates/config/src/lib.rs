use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use oma_contract::{AgentSummary, ApprovalMode, ModelInfo};
use oma_mcp::McpServerConfig;
use oma_provider::ModelConfig;
pub use oma_provider::{ModelEntry, ProviderConfig};
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
    pub default_model:         String,
    #[serde(default = "default_agent_str")]
    pub default_agent:         String,
    #[serde(default)]
    pub default_approval_mode: ApprovalMode,
    #[serde(default)]
    pub server:                ServerConfig,
    #[serde(default)]
    pub providers:             BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub mcp_servers:           BTreeMap<String, McpServerConfig>,
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
            default_model:         default_model_str(),
            default_agent:         default_agent_str(),
            default_approval_mode: ApprovalMode::Normal,
            server:                ServerConfig::default(),
            providers:             BTreeMap::new(),
            mcp_servers:           BTreeMap::new(),
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

    /// 配置文件标准路径 (~/.config/oma/config.toml)
    pub fn config_path() -> Option<PathBuf> {
        dirs_config_dir().map(|d| d.join("oma").join("config.toml"))
    }

    /// 自动从标准路径加载配置（若不存在则返回默认配置）
    pub fn load_or_default() -> Self {
        if let Some(path) = Self::config_path() {
            if path.exists() {
                if let Ok(cfg) = Self::load_from_file(&path) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    /// 序列化并回写配置文件（前端配置修改后的持久化入口）
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;
        std::fs::write(path, content).with_context(|| format!("Failed to write config {}", path.display()))?;
        Ok(())
    }

    /// 根据 "provider/model" 字符串定位 Provider 与 Model 配置
    pub fn find_model(&self, selector: &str) -> Option<(&ProviderConfig, ModelConfig)> {
        let mut parts = selector.splitn(2, '/');
        let provider_name = parts.next()?;
        let model_id = parts.next().unwrap_or(provider_name);

        let provider = self.providers.get(provider_name)?;

        // 优先使用配置的 models 清单；未配置该 id 或清单为空时，动态构造默认 ModelConfig
        let model_cfg = provider
            .models
            .iter()
            .find(|m| m.id == model_id)
            .map(|m| ModelConfig {
                id:                m.id.clone(),
                name:              if m.name.is_empty() {
                    m.id.clone()
                } else {
                    m.name.clone()
                },
                context_len:       m.context_len,
                supports_vision:   m.supports_vision,
                supports_thinking: m.supports_thinking,
                headers:           BTreeMap::new(),
                body:              serde_json::json!({}),
            })
            .unwrap_or_else(|| ModelConfig {
                id:                model_id.to_string(),
                name:              model_id.to_string(),
                context_len:       128_000,
                supports_vision:   true,
                supports_thinking: true,
                headers:           BTreeMap::new(),
                body:              serde_json::json!({}),
            });
        Some((provider, model_cfg))
    }

    /// 各 provider 配置的模型元数据（供握手协议枚举真实模型）
    pub fn model_catalog(&self) -> BTreeMap<String, Vec<ModelInfo>> {
        self.providers
            .iter()
            .map(|(p_id, p_cfg)| {
                let models = p_cfg
                    .models
                    .iter()
                    .map(|m| ModelInfo {
                        id:                m.id.clone(),
                        name:              if m.name.is_empty() {
                            m.id.clone()
                        } else {
                            m.name.clone()
                        },
                        context_len:       m.context_len,
                        supports_vision:   m.supports_vision,
                        supports_thinking: m.supports_thinking,
                    })
                    .collect::<Vec<_>>();
                (p_id.clone(), models)
            })
            .collect()
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
    pub id:                 String,
    pub name:               String,
    pub description:        String,
    #[serde(default)]
    pub tools:              Vec<String>,
    pub system_prompt_body: String,
}

/// Agent Frontmatter 头部结构
#[derive(Debug, Deserialize)]
struct Frontmatter {
    name:        String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools:       Vec<String>,
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
                id:                 id.to_string(),
                name:               fm.name,
                description:        fm.description,
                tools:              fm.tools,
                system_prompt_body: body.to_string(),
            });
        }
    }

    Ok(AgentTemplate {
        id:                 id.to_string(),
        name:               id.to_string(),
        description:        format!("Agent {}", id),
        tools:              Vec::new(),
        system_prompt_body: raw.to_string(),
    })
}

/// Agent 模板加载器
pub struct AgentLoader;

impl AgentLoader {
    /// 加载指定 Agent 模板（项目目录 > 全局用户目录 > 二进制内嵌）
    pub fn load_agent(agent_id: &str, workspace: &Path) -> Result<AgentTemplate> {
        // 1. 项目级覆盖: <workspace>/.oma/agents/<agent_id>.md
        let project_path = workspace
            .join(".oma")
            .join("agents")
            .join(format!("{}.md", agent_id));
        if project_path.exists() {
            if let Ok(raw) = std::fs::read_to_string(&project_path) {
                return parse_markdown_template(agent_id, &raw);
            }
        }

        // 2. 用户全局配置: ~/.config/oma/agents/<agent_id>.md
        if let Some(config_dir) = dirs_config_dir() {
            let user_path = config_dir
                .join("oma")
                .join("agents")
                .join(format!("{}.md", agent_id));
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
                    id:          tmpl.id,
                    name:        tmpl.name,
                    description: tmpl.description,
                });
            }
        }

        list
    }

    /// 动态拼装注入实时环境块的最终 System Prompt
    pub fn build_system_prompt(template: &AgentTemplate, workspace: &Path, active_model: &str) -> String {
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

    #[test]
    fn test_configured_models_override_synthesis() {
        let toml_str = r#"
[providers.p1]
api_type = "completion"
base_url = "http://localhost/v1"
api_key = "k"
[[providers.p1.models]]
id = "m1"
name = "Model One"
context_len = 1_048_576
supports_vision = false
[[providers.p1.models]]
id = "m2"

[providers.p2]
api_type = "anthropic"
base_url = "http://localhost/v1"
api_key = "k"
"#;
        let config: OmaConfig = toml::from_str(toml_str).unwrap();

        // 配置清单内的模型: 精确元数据
        let (_, m1) = config.find_model("p1/m1").unwrap();
        assert_eq!(m1.name, "Model One");
        assert_eq!(m1.context_len, 1_048_576);
        assert!(!m1.supports_vision);
        // name 缺省时回退为 id
        let (_, m2) = config.find_model("p1/m2").unwrap();
        assert_eq!(m2.name, "m2");
        assert_eq!(m2.context_len, 128_000);
        // 清单外的 id: 动态合成 (向后兼容)
        let (_, mx) = config.find_model("p1/not-listed").unwrap();
        assert_eq!(mx.context_len, 128_000);
        assert!(mx.supports_thinking);
        // 无清单 provider: 全部合成
        let (_, p2m) = config.find_model("p2/anything").unwrap();
        assert_eq!(p2m.id, "anything");

        // 握手目录: 配置清单原样枚举, 无清单 provider 为空列表
        let catalog = config.model_catalog();
        assert_eq!(catalog["p1"].len(), 2);
        assert_eq!(catalog["p1"][0].id, "m1");
        assert_eq!(catalog["p1"][0].context_len, 1_048_576);
        assert!(catalog["p2"].is_empty());
    }

    #[test]
    fn test_save_and_reload_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("oma").join("config.toml");

        let mut providers = BTreeMap::new();
        providers.insert(
            "deepseek".into(),
            ProviderConfig {
                api_type: "completion".into(),
                base_url: "https://api.deepseek.com/v1".into(),
                api_key:  "env:DEEPSEEK_KEY".into(),
                headers:  BTreeMap::new(),
                body:     serde_json::json!({}),
                models:   Vec::new(),
            },
        );
        let cfg = OmaConfig {
            default_model: "deepseek/deepseek-chat".into(),
            providers,
            ..OmaConfig::default()
        };
        cfg.save_to_file(&path).unwrap();

        let reloaded = OmaConfig::load_from_file(&path).unwrap();
        assert_eq!(reloaded.default_model, "deepseek/deepseek-chat");
        assert!(reloaded.providers.contains_key("deepseek"));
    }
}
