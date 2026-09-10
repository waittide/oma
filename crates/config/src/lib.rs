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

/// Catppuccin 全部 accent label (供主题设置校验)
pub const THEME_ACCENTS: [&str; 14] = [
    "rosewater",
    "flamingo",
    "pink",
    "mauve",
    "red",
    "maroon",
    "peach",
    "yellow",
    "green",
    "teal",
    "sky",
    "sapphire",
    "blue",
    "lavender",
];

/// 前端主题设置 (Catppuccin 体系；浅色/深色均可选内置或自定义主题)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    /// 显示模式: "light" | "dark" | "system"
    #[serde(default = "default_theme_mode")]
    pub mode:        String,
    /// 深色主题 id: 内置 "frappe"|"macchiato"|"mocha"，或自定义主题 id
    #[serde(default = "default_dark_flavor")]
    pub dark_flavor: String,
    /// 浅色主题 id: 内置 "latte"，或自定义主题 id
    #[serde(default = "default_light_theme")]
    pub light_theme: String,
    /// 强调色 label (THEME_ACCENTS 之一)
    #[serde(default = "default_accent")]
    pub accent:      String,
}

fn default_theme_mode() -> String {
    "dark".to_string()
}
fn default_dark_flavor() -> String {
    "mocha".to_string()
}
fn default_light_theme() -> String {
    "latte".to_string()
}
fn default_accent() -> String {
    "blue".to_string()
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            mode:        default_theme_mode(),
            dark_flavor: default_dark_flavor(),
            light_theme: default_light_theme(),
            accent:      default_accent(),
        }
    }
}

impl Theme {
    /// 校验全部 label；custom 为用户自定义主题清单 (写入侧闸门)
    pub fn validate(&self, custom: &[CustomTheme]) -> Result<()> {
        if !matches!(self.mode.as_str(), "light" | "dark" | "system") {
            anyhow::bail!("invalid theme.mode {:?}: expect light|dark|system", self.mode);
        }
        let dark_ok = matches!(self.dark_flavor.as_str(), "frappe" | "macchiato" | "mocha")
            || custom
                .iter()
                .any(|t| t.id == self.dark_flavor && t.mode == "dark");
        if !dark_ok {
            anyhow::bail!("invalid theme.dark_flavor {:?}", self.dark_flavor);
        }
        let light_ok = matches!(self.light_theme.as_str(), "latte")
            || custom
                .iter()
                .any(|t| t.id == self.light_theme && t.mode == "light");
        if !light_ok {
            anyhow::bail!("invalid theme.light_theme {:?}", self.light_theme);
        }
        if !THEME_ACCENTS.contains(&self.accent.as_str()) {
            anyhow::bail!(
                "invalid theme.accent {:?}: expect one of {:?}",
                self.accent,
                THEME_ACCENTS
            );
        }
        Ok(())
    }
}

/// 用户自定义主题：以内置 flavor 为基底的调色板覆盖（浅色/深色均可多套）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomTheme {
    /// 稳定 slug，被 theme.dark_flavor / theme.light_theme 引用
    pub id:     String,
    pub name:   String,
    /// light | dark：决定该主题归属的候选组
    pub mode:   String,
    /// 继承的内置 flavor：light 系 latte；dark 系 frappe|macchiato|mocha
    pub base:   String,
    /// Catppuccin 令牌覆盖（键不含 "--"，如 base/mantle/text/surface0…）
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
}

impl CustomTheme {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !self.id.is_empty(),
            "invalid custom theme id {:?}",
            self.id
        );
        anyhow::ensure!(!self.name.trim().is_empty(), "custom theme '{}' name is empty", self.id);
        anyhow::ensure!(
            matches!(self.mode.as_str(), "light" | "dark"),
            "invalid custom theme '{}' mode {:?}: expect light|dark",
            self.id,
            self.mode
        );
        let allowed = match self.mode.as_str() {
            "light" => "latte",
            _ => "frappe|macchiato|mocha",
        };
        let base_ok = match self.mode.as_str() {
            "light" => self.base == "latte",
            _ => matches!(self.base.as_str(), "frappe" | "macchiato" | "mocha"),
        };
        anyhow::ensure!(
            base_ok,
            "invalid custom theme '{}' base {:?}: expect {}",
            self.id,
            self.base,
            allowed
        );
        Ok(())
    }
}

/// Oma 根配置文件 (~/.config/oma/config.toml)
///
/// `deny_unknown_fields` 让拼错的键名与前端字段映射错误立即报错，
/// 而不是被静默忽略后「保存成功但配置没变」。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmaConfig {
    #[serde(default = "default_model_str")]
    pub default_model:         String,
    #[serde(default = "default_agent_str")]
    pub default_agent:         String,
    #[serde(default)]
    pub default_approval_mode: ApprovalMode,
    #[serde(default)]
    pub theme:                 Theme,
    #[serde(default)]
    pub server:                ServerConfig,
    #[serde(default)]
    pub providers:             BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub mcp_servers:           BTreeMap<String, McpServerConfig>,
    #[serde(default)]
    pub custom_themes:         Vec<CustomTheme>,
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
            theme:                 Theme::default(),
            server:                ServerConfig::default(),
            providers:             BTreeMap::new(),
            mcp_servers:           BTreeMap::new(),
            custom_themes:         Vec::new(),
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

    /// 原子回写：先落同目录临时文件并 fsync，再 rename 覆盖。
    /// 避免进程中断留下半截配置，导致下次启动解析失败。
    pub fn save_to_file_atomic(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config dir {}", parent.display()))?;
        }
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "config.toml".to_string());
        let tmp = path.with_file_name(format!(".{}.tmp", file_name));
        {
            let mut file = std::fs::File::create(&tmp)
                .with_context(|| format!("Failed to create temp config {}", tmp.display()))?;
            std::io::Write::write_all(&mut file, content.as_bytes())
                .with_context(|| format!("Failed to write temp config {}", tmp.display()))?;
            file.sync_all()
                .with_context(|| format!("Failed to sync temp config {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("Failed to replace config {}", path.display()))?;
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
                max_output:        m.max_output,
                reasoning_effort:  m.reasoning_effort.clone(),
                headers:           BTreeMap::new(),
                body:              serde_json::json!({}),
            })
            .unwrap_or_else(|| ModelConfig {
                id:                model_id.to_string(),
                name:              model_id.to_string(),
                context_len:       128_000,
                supports_vision:   true,
                supports_thinking: true,
                max_output:        None,
                reasoning_effort:  String::new(),
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
                        max_output:        m.max_output,
                        reasoning_effort:  m.reasoning_effort.clone(),
                        input_types:       m.input_types.clone(),
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

/// Agent 预设文件条目（内置模板或用户覆盖）：scope = bundled | global | project
#[derive(Debug, Clone, Serialize)]
pub struct AgentFile {
    pub id:          String,
    pub name:        String,
    pub description: String,
    pub tools:       Vec<String>,
    pub scope:       String,
    /// 完整 Markdown 原文（含 frontmatter）
    pub content:     String,
}

/// 技能文件条目：scope = global | project
///
/// 技能与 Agent 预设是两件事：预设决定「以什么角色、能用哪些工具运行」，
/// 技能是一段可复用领域知识，按需读取而不占用常驻上下文。
#[derive(Debug, Clone, Serialize)]
pub struct SkillFile {
    pub id:          String,
    pub name:        String,
    pub description: String,
    pub scope:       String,
    /// 完整 Markdown 原文（含 frontmatter）
    pub content:     String,
    /// 磁盘绝对路径：目录注入 System Prompt 时供模型用 read 工具取用
    pub path:        String,
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
                .with_context(|| format!("Failed to parse YAML frontmatter for {}", id))?;

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
        Self::list_agent_files(Some(workspace))
            .into_iter()
            .map(|a| AgentSummary {
                id:          a.id,
                name:        a.name,
                description: a.description,
            })
            .collect()
    }

    /// 动态拼装注入实时环境块与技能目录的最终 System Prompt
    pub fn build_system_prompt(template: &AgentTemplate, workspace: &Path, active_model: &str) -> String {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        let mut prompt = format!(
            "{}\n\n<runtime_context>\n- Workspace: {}\n- Operating System: {} ({})\n- Today: {}\n- Active Model: {}\n</runtime_context>",
            template.system_prompt_body,
            workspace.display(),
            os,
            arch,
            today,
            active_model
        );

        // 技能只在目录里列名与路径，需要时由模型自行 read：
        // 常驻全文会持续挤占上下文，而多数轮次用不到任何技能
        if let Some(catalog) = SkillLoader::catalog(workspace) {
            prompt.push_str("\n\n");
            prompt.push_str(&catalog);
        }
        prompt
    }

    /// 全局 Agent 预设目录 (~/.config/oma/agents)
    pub fn global_agents_dir() -> Option<PathBuf> {
        dirs_config_dir().map(|d| d.join("oma").join("agents"))
    }

    /// 项目 Agent 预设目录 (<workspace>/.oma/agents)
    pub fn project_agents_dir(workspace: &Path) -> PathBuf {
        workspace.join(".oma").join("agents")
    }

    /// 解析预设 Markdown 为条目；frontmatter 损坏时退化为「id 即名称」仍可见
    fn agent_file_from_raw(id: &str, scope: &str, raw: &str) -> AgentFile {
        match parse_markdown_template(id, raw) {
            Ok(t) => AgentFile {
                id:          id.to_string(),
                name:        t.name,
                description: t.description,
                tools:       t.tools,
                scope:       scope.to_string(),
                content:     raw.to_string(),
            },
            Err(_) => AgentFile {
                id:          id.to_string(),
                name:        id.to_string(),
                description: String::new(),
                tools:       Vec::new(),
                scope:       scope.to_string(),
                content:     raw.to_string(),
            },
        }
    }

    fn agent_files_in_dir(dir: &Path, scope: &str) -> Vec<AgentFile> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if let Ok(raw) = std::fs::read_to_string(&path) {
                    out.push(Self::agent_file_from_raw(id, scope, &raw));
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// 列出全部 Agent 预设：内置 + 全局 + 项目（同 id 时项目覆盖全局、全局覆盖内置）
    pub fn list_agent_files(workspace: Option<&Path>) -> Vec<AgentFile> {
        let mut by_id: std::collections::BTreeMap<String, AgentFile> = std::collections::BTreeMap::new();
        for (id, raw) in BUNDLED_AGENTS {
            by_id.insert(id.to_string(), Self::agent_file_from_raw(id, "bundled", raw));
        }
        if let Some(dir) = Self::global_agents_dir() {
            for a in Self::agent_files_in_dir(&dir, "global") {
                by_id.insert(a.id.clone(), a);
            }
        }
        if let Some(ws) = workspace {
            for a in Self::agent_files_in_dir(&Self::project_agents_dir(ws), "project") {
                by_id.insert(a.id.clone(), a);
            }
        }
        by_id.into_values().collect()
    }

    /// 读取单个 Agent 预设
    pub fn read_agent_file(workspace: Option<&Path>, id: &str) -> Result<AgentFile> {
        Self::list_agent_files(workspace)
            .into_iter()
            .find(|a| a.id == id)
            .with_context(|| format!("Agent preset '{}' not found", id))
    }

    /// 写入 Agent 预设：scope = global | project；内置模板只读
    pub fn write_agent_file(
        workspace: Option<&Path>,
        id: &str,
        scope: &str,
        name: &str,
        description: &str,
        tools: &[String],
        content: &str,
    ) -> Result<PathBuf> {
        anyhow::ensure!(
            matches!(scope, "global" | "project"),
            "Invalid preset scope {:?}: expect global|project",
            scope
        );
        anyhow::ensure!(
            !BUNDLED_AGENTS.iter().any(|(bid, _)| *bid == id),
            "Bundled preset '{}' is read-only",
            id
        );
        anyhow::ensure!(is_valid_slug(id), "Invalid preset id {:?}", id);

        let dir = match scope {
            "global" => Self::global_agents_dir().with_context(|| "Cannot resolve global config dir for presets")?,
            _ => {
                let ws = workspace.with_context(|| "workspace is required for project presets")?;
                Self::project_agents_dir(ws)
            }
        };
        std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create presets dir {}", dir.display()))?;
        let path = dir.join(format!("{}.md", id));
        std::fs::write(&path, render_markdown(name, description, tools, content)?)
            .with_context(|| format!("Failed to write preset {}", path.display()))?;
        Ok(path)
    }

    /// 删除 Agent 预设文件（仅限 global/project 作用域）
    pub fn delete_agent_file(workspace: Option<&Path>, id: &str, scope: &str) -> Result<()> {
        let dir = match scope {
            "global" => Self::global_agents_dir().with_context(|| "Cannot resolve global config dir for presets")?,
            "project" => {
                let ws = workspace.with_context(|| "workspace is required for project presets")?;
                Self::project_agents_dir(ws)
            }
            other => anyhow::bail!("Invalid preset scope {:?}: expect global|project", other),
        };
        let path = dir.join(format!("{}.md", id));
        anyhow::ensure!(path.exists(), "Preset file {} not found", path.display());
        std::fs::remove_file(&path).with_context(|| format!("Failed to delete preset {}", path.display()))
    }
}

/// 技能加载器：扫描 `~/.config/oma/skills` 与 `<workspace>/.oma/skills` 下的 Markdown。
///
/// 技能是可按需取用的领域知识，与 Agent 预设（角色 + 工具白名单）是两套独立机制。
pub struct SkillLoader;

impl SkillLoader {
    /// 全局技能目录 (~/.config/oma/skills)
    pub fn global_dir() -> Option<PathBuf> {
        dirs_config_dir().map(|d| d.join("oma").join("skills"))
    }

    /// 项目技能目录 (<workspace>/.oma/skills)
    pub fn project_dir(workspace: &Path) -> PathBuf {
        workspace.join(".oma").join("skills")
    }

    fn skill_from_path(path: &Path, scope: &str) -> Option<SkillFile> {
        let id = path.file_stem()?.to_str()?.to_string();
        let raw = std::fs::read_to_string(path).ok()?;
        let (name, description) = match parse_markdown_template(&id, &raw) {
            Ok(t) => (t.name, t.description),
            Err(_) => (id.clone(), String::new()),
        };
        Some(SkillFile {
            id,
            name,
            description,
            scope: scope.to_string(),
            content: raw,
            path: path.to_string_lossy().to_string(),
        })
    }

    fn skills_in_dir(dir: &Path, scope: &str) -> Vec<SkillFile> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                if let Some(skill) = Self::skill_from_path(&path, scope) {
                    out.push(skill);
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// 列出全部技能（同 id 时项目覆盖全局）
    pub fn list_skills(workspace: Option<&Path>) -> Vec<SkillFile> {
        let mut by_id: std::collections::BTreeMap<String, SkillFile> = std::collections::BTreeMap::new();
        if let Some(dir) = Self::global_dir() {
            for s in Self::skills_in_dir(&dir, "global") {
                by_id.insert(s.id.clone(), s);
            }
        }
        if let Some(ws) = workspace {
            for s in Self::skills_in_dir(&Self::project_dir(ws), "project") {
                by_id.insert(s.id.clone(), s);
            }
        }
        by_id.into_values().collect()
    }

    /// 读取单个技能
    pub fn read_skill(workspace: Option<&Path>, id: &str) -> Result<SkillFile> {
        Self::list_skills(workspace)
            .into_iter()
            .find(|s| s.id == id)
            .with_context(|| format!("Skill '{}' not found", id))
    }

    /// 写入技能：scope = global | project
    pub fn write_skill(
        workspace: Option<&Path>,
        id: &str,
        scope: &str,
        name: &str,
        description: &str,
        content: &str,
    ) -> Result<PathBuf> {
        anyhow::ensure!(
            matches!(scope, "global" | "project"),
            "Invalid skill scope {:?}: expect global|project",
            scope
        );
        anyhow::ensure!(is_valid_slug(id), "Invalid skill id {:?}", id);

        let dir = match scope {
            "global" => Self::global_dir().with_context(|| "Cannot resolve global config dir for skills")?,
            _ => {
                let ws = workspace.with_context(|| "workspace is required for project skills")?;
                Self::project_dir(ws)
            }
        };
        std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create skills dir {}", dir.display()))?;
        let path = dir.join(format!("{}.md", id));
        std::fs::write(&path, render_markdown(name, description, &[], content)?)
            .with_context(|| format!("Failed to write skill {}", path.display()))?;
        Ok(path)
    }

    /// 删除技能文件（仅限 global/project 作用域）
    pub fn delete_skill(workspace: Option<&Path>, id: &str, scope: &str) -> Result<()> {
        let dir = match scope {
            "global" => Self::global_dir().with_context(|| "Cannot resolve global config dir for skills")?,
            "project" => {
                let ws = workspace.with_context(|| "workspace is required for project skills")?;
                Self::project_dir(ws)
            }
            other => anyhow::bail!("Invalid skill scope {:?}: expect global|project", other),
        };
        let path = dir.join(format!("{}.md", id));
        anyhow::ensure!(path.exists(), "Skill file {} not found", path.display());
        std::fs::remove_file(&path).with_context(|| format!("Failed to delete skill {}", path.display()))
    }

    /// 组装注入 System Prompt 的技能目录；无技能时返回 None
    pub fn catalog(workspace: &Path) -> Option<String> {
        let skills = Self::list_skills(Some(workspace));
        if skills.is_empty() {
            return None;
        }
        let mut out = String::from(
            "<available_skills>\n以下技能是按需取用的领域知识。当任务与某个技能相关时，\n先用 read 工具读取其文件再照此执行；不相关时无需读取。\n",
        );
        for s in &skills {
            let summary = if s.description.is_empty() {
                &s.name
            } else {
                &s.description
            };
            out.push_str(&format!("- {}: {} (file: {})\n", s.id, summary, s.path));
        }
        out.push_str("</available_skills>");
        Some(out)
    }
}

/// 内置 Agent 预设清单（id, 模板源码）
pub const BUNDLED_AGENTS: [(&str, &str); 5] = [
    ("task", TEMPLATE_TASK),
    ("plan", TEMPLATE_PLAN),
    ("explore", TEMPLATE_EXPLORE),
    ("review", TEMPLATE_REVIEW),
    ("build", TEMPLATE_BUILD),
];

/// 预设/技能标识符：字母数字与 - _，非空
fn is_valid_slug(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Frontmatter 序列化结构；由 serde_yaml 负责转义，
/// 避免描述含冒号/引号/换行时写出无法回读的 YAML
#[derive(Serialize)]
struct FrontmatterOut<'a> {
    name:        &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    description: &'a str,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    tools:       &'a [String],
}

/// 渲染带 frontmatter 的 Markdown 文件
fn render_markdown(name: &str, description: &str, tools: &[String], content: &str) -> Result<String> {
    let frontmatter = serde_yaml::to_string(&FrontmatterOut {
        name,
        description,
        tools,
    })?;
    Ok(format!("---\n{}---\n\n{}", frontmatter, content.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_theme_validation() {
        let mut custom = vec![CustomTheme {
            id:     "nord-dark".into(),
            name:   "Nord Dark".into(),
            mode:   "dark".into(),
            base:   "mocha".into(),
            colors: BTreeMap::new(),
        }];

        // 深色引用自定义主题合法；浅色引用深色主题非法
        let dark = Theme {
            mode: "dark".into(),
            dark_flavor: "nord-dark".into(),
            ..Theme::default()
        };
        assert!(dark.validate(&custom).is_ok());
        let wrong_mode = Theme {
            mode: "light".into(),
            light_theme: "nord-dark".into(),
            ..Theme::default()
        };
        assert!(wrong_mode.validate(&custom).is_err());

        // 浅色自定义主题被 light_theme 引用合法
        custom.push(CustomTheme {
            id:     "nord-light".into(),
            name:   "Nord Light".into(),
            mode:   "light".into(),
            base:   "latte".into(),
            colors: BTreeMap::new(),
        });
        let light = Theme {
            mode: "light".into(),
            light_theme: "nord-light".into(),
            ..Theme::default()
        };
        assert!(light.validate(&custom).is_ok());

        // 非法 base 被拒绝
        custom.push(CustomTheme {
            id:     "bad".into(),
            name:   "Bad".into(),
            mode:   "dark".into(),
            base:   "latte".into(),
            colors: BTreeMap::new(),
        });
        assert!(custom.last().unwrap().validate().is_err());
    }

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

    #[test]
    fn test_agent_preset_crud_scopes() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();

        // 写入与列表（project 作用域：测试不得触碰用户真实的全局配置）
        AgentLoader::write_agent_file(
            Some(&ws),
            "my-preset",
            "project",
            "My Preset",
            "Does things",
            &["read".to_string()],
            "Do the thing.",
        )
        .unwrap();
        let all = AgentLoader::list_agent_files(Some(&ws));
        let g = all.iter().find(|a| a.id == "my-preset").unwrap();
        assert_eq!(g.scope, "project");
        assert_eq!(g.name, "My Preset");
        assert_eq!(g.tools, vec!["read"]);

        // 项目覆盖同名 id
        AgentLoader::write_agent_file(
            Some(&ws),
            "my-preset",
            "project",
            "Proj Preset",
            "Project variant",
            &[],
            "Project body.",
        )
        .unwrap();
        let overridden = AgentLoader::read_agent_file(Some(&ws), "my-preset").unwrap();
        assert_eq!(overridden.scope, "project");
        assert_eq!(overridden.name, "Proj Preset");

        // 内置预设只读
        assert!(AgentLoader::write_agent_file(Some(&ws), "task", "global", "X", "", &[], "body").is_err());

        // 删除后回落到内置兜底
        AgentLoader::delete_agent_file(Some(&ws), "my-preset", "project").unwrap();
        assert!(AgentLoader::read_agent_file(Some(&ws), "my-preset").is_err());
        // 内置兜底仍然可用
        assert!(AgentLoader::read_agent_file(Some(&ws), "task").is_ok());
    }

    /// 技能与 Agent 预设必须是两套互不干扰的存储：同名也不会互相覆盖。
    ///
    /// 全部使用 project 作用域，避免与共享的全局配置目录相互干扰。
    #[test]
    fn test_skills_are_separate_from_presets() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();

        // 项目作用域初始无技能；内置预设始终可见
        let project_skills = |ws: &std::path::Path| -> Vec<SkillFile> {
            SkillLoader::list_skills(Some(ws))
                .into_iter()
                .filter(|s| s.scope == "project")
                .collect()
        };
        assert!(project_skills(&ws).is_empty());
        assert!(!AgentLoader::list_agent_files(Some(&ws)).is_empty());

        SkillLoader::write_skill(Some(&ws), "task", "project", "同名技能", "与预设同名", "技能正文").unwrap();
        let skills = project_skills(&ws);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "task");
        assert_eq!(skills[0].scope, "project");

        // 同名互不覆盖：预设仍是内置只读模板，且工具白名单来自预设而非技能
        let preset = AgentLoader::read_agent_file(Some(&ws), "task").unwrap();
        assert_eq!(preset.scope, "bundled");
        assert_ne!(preset.name, "同名技能");
        assert!(preset.tools.contains(&"write".to_string()));

        // 技能存放在独立的 skills 目录下
        let skill_dir = SkillLoader::project_dir(&ws);
        let preset_dir = AgentLoader::project_agents_dir(&ws);
        assert_ne!(skill_dir, preset_dir);
        assert!(skill_dir.join("task.md").exists());
        assert!(!preset_dir.join("task.md").exists());
    }

    /// 描述含冒号等 YAML 元字符时必须能原样回读。
    #[test]
    fn test_frontmatter_escaping_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();

        let tricky = "用法: 先读 a.md, 再执行 \"步骤\"";
        SkillLoader::write_skill(Some(&ws), "tricky", "project", "Tricky: 名称", tricky, "正文").unwrap();
        let skill = SkillLoader::read_skill(Some(&ws), "tricky").unwrap();
        assert_eq!(skill.name, "Tricky: 名称");
        assert_eq!(skill.description, tricky);

        AgentLoader::write_agent_file(
            Some(&ws),
            "preset-tricky",
            "project",
            "Name: with colon",
            "描述: 含冒号",
            &["read".to_string()],
            "body",
        )
        .unwrap();
        let preset = AgentLoader::read_agent_file(Some(&ws), "preset-tricky").unwrap();
        assert_eq!(preset.name, "Name: with colon");
        assert_eq!(preset.description, "描述: 含冒号");
        assert_eq!(preset.tools, vec!["read"]);
    }

    /// 技能通过目录注入 System Prompt，携带可读取的绝对路径。
    ///
    /// 使用 project 作用域，避免依赖（或污染）用户共享的全局技能目录。
    #[test]
    fn test_skill_catalog_injected_into_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();

        let preset = AgentLoader::load_agent("task", &ws).unwrap();
        // 写入前该技能不应出现在目录中
        let before = AgentLoader::build_system_prompt(&preset, &ws, "p/m");
        assert!(!before.contains("- cargo:"));

        SkillLoader::write_skill(Some(&ws), "cargo", "project", "Cargo", "Rust 构建约定", "正文").unwrap();
        let with_skills = AgentLoader::build_system_prompt(&preset, &ws, "p/m");
        assert!(with_skills.contains("<available_skills>"));
        assert!(with_skills.contains("- cargo: Rust 构建约定"));
        // 目录里给的是真实路径，模型才能用 read 取用
        let expected = SkillLoader::read_skill(Some(&ws), "cargo").unwrap().path;
        assert!(with_skills.contains(&expected), "catalog must expose the skill path");
        assert!(std::path::Path::new(&expected).exists());
        // runtime 环境块仍然存在
        assert!(with_skills.contains("<runtime_context>"));
    }

    #[test]
    fn test_skill_dirs_resolve() {
        // 两个作用域目录都应可解析，且与预设目录分离
        assert!(SkillLoader::global_dir().is_some());
        let ws = std::path::Path::new("/tmp/ws");
        assert!(SkillLoader::project_dir(ws).ends_with(".oma/skills"));
        assert!(AgentLoader::project_agents_dir(ws).ends_with(".oma/agents"));
    }

    /// 配置键拼错必须报错，而不是被静默忽略。
    #[test]
    fn test_unknown_config_field_is_rejected() {
        let err = toml::from_str::<OmaConfig>(
            r#"
default_model = "p/m"
model = "p/other"
"#,
        );
        assert!(err.is_err(), "unknown key must not be silently ignored");
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("model"), "error should name the offending key: {}", msg);

        // 合法配置不受影响
        assert!(toml::from_str::<OmaConfig>("default_model = \"p/m\"").is_ok());
    }
}
