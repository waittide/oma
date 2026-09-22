use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use oma_contract::{ACCENTS, AgentSummary, McpServerConfig, ModelInfo, Palette, PaletteMode, ResolvedTheme, Theme};
pub use oma_contract::{PALETTE_TOKENS, is_valid_hex_color};
use oma_provider::ModelConfig;
pub use oma_provider::{ModelEntry, ProviderConfig};
use serde::{Deserialize, Serialize};

/// 默认监听主机：只绑回环地址，装完不会把 Agent 暴露到局域网。
pub const DEFAULT_SERVER_HOST: &str = "127.0.0.1";

/// 默认监听端口
pub const DEFAULT_SERVER_PORT: u16 = 17431;

/// 默认监听地址 `host:port`。
///
/// 仅供客户端（`oma status` 默认目标、TUI 无可用连接时的兜底）与
/// `oma daemon --addr` 的取值使用；服务端绑定走 `ServerConfig` 的 `host` / `port`
/// 两个字段，不做字符串拼接，以免 IPv6 字面量被拼坏。
pub fn default_server_addr() -> String {
    format!("{}:{}", DEFAULT_SERVER_HOST, DEFAULT_SERVER_PORT)
}

/// 未配置访问 token 时写入配置文件的默认值。
///
/// 默认只监听回环地址，本地访问本来就不需要密钥；这里给一个固定值是为了
/// 「装完就能用」，需要暴露到局域网/公网时必须自行改掉。
pub const DEFAULT_AUTH_TOKEN: &str = "admin";

/// 服务端配置
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// 监听主机；默认只绑回环地址
    #[serde(default = "default_server_host")]
    pub host:  String,
    /// 监听端口
    #[serde(default = "default_server_port")]
    pub port:  u16,
    /// 访问 token：REST 需 `Authorization: Bearer`，WS 握手可用 `?token=`。
    ///
    /// 缺省为空表示「尚未设置」：启动时会向配置文件补写
    /// [`DEFAULT_AUTH_TOKEN`]，这样用户能在 settings.json 里直接看到并修改。
    #[serde(default)]
    pub token: String,
}

fn default_server_host() -> String {
    DEFAULT_SERVER_HOST.to_string()
}

fn default_server_port() -> u16 {
    DEFAULT_SERVER_PORT
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host:  default_server_host(),
            port:  default_server_port(),
            token: String::new(),
        }
    }
}

/// 内置调色板源码：与 `AgentLoader` 的内嵌模板同理，打包进二进制作为兜底，
/// 用户目录只放自己的调色板，不需要任何初始化写入。
///
/// 内置四套均为 Catppuccin 族：默认主题引用 `mocha` / `latte`——同一套语义色的
/// 深色与浅色版本，令牌名与取值一一对应，切换明暗时不会有语义色断层；
/// 另外两套深色 `frappe` / `macchiato` 只换色阶深浅，令牌名沿用同一批。
pub const BUILTIN_PALETTES: &[(&str, &str)] = &[
    ("latte", include_str!("themes/latte.json")),
    ("frappe", include_str!("themes/frappe.json")),
    ("macchiato", include_str!("themes/macchiato.json")),
    ("mocha", include_str!("themes/mocha.json")),
];

/// 校验调色板 id 是否为合法 slug（同时决定文件名，故不允许路径分隔符）。
pub fn is_valid_palette_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 调色板加载器。
///
/// 内置调色板编译期内嵌；用户调色板位于 `<配置目录>/oma/themes/<id>.json`。
/// 内置 id 为保留位（同名文件既不会加载也无法写入，见 `is_builtin`），
/// 因此列表顺序稳定：内置在前，用户自定义按目录枚举顺序追加在后。
pub struct PaletteLoader;

impl PaletteLoader {
    /// 用户调色板目录 (~/.config/oma/themes)
    pub fn user_themes_dir() -> Option<PathBuf> {
        dirs_config_dir().map(|d| d.join("oma").join("themes"))
    }

    /// 解析单份调色板 JSON；`fallback_id` 为文件名派生的 id（JSON 可省略 id）。
    fn parse(raw: &str, fallback_id: &str) -> Result<Palette> {
        let mut palette: Palette =
            serde_json::from_str(raw).with_context(|| format!("failed to parse palette '{}'", fallback_id))?;
        if palette.id.trim().is_empty() {
            palette.id = fallback_id.to_string();
        }
        Ok(palette)
    }

    /// 全部内置调色板（按 BUILTIN_PALETTES 声明顺序）。
    pub fn builtin_all() -> Vec<Palette> {
        BUILTIN_PALETTES
            .iter()
            .filter_map(|(id, raw)| match Self::parse(raw, id) {
                // 内置数据是编译期常量，解析失败只可能是构建产物损坏
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::error!(palette = id, error = %e, "bundled palette is unparsable");
                    None
                }
            })
            .collect()
    }

    /// 列出全部可用调色板：内置 + 用户目录，用户同名覆盖内置。
    ///
    /// 单个损坏文件只跳过并告警，不使整个列表不可用。
    pub fn list() -> Vec<Palette> {
        let mut palettes = Self::builtin_all();
        if let Some(dir) = Self::user_themes_dir() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    let Ok(raw) = std::fs::read_to_string(&path) else {
                        continue;
                    };
                    match Self::parse(&raw, id) {
                        Ok(p) => match palettes.iter_mut().find(|e| e.id == p.id) {
                            // 已存在的 id 只可能是内置（save 拒绝了内置 id 的落盘），
                            // 此处不得覆盖：否则手写一个 mocha.json 就能改掉内置配色。
                            Some(_) => tracing::warn!(palette = id, "ignoring user palette that shadows a bundled id"),
                            None => palettes.push(p),
                        },
                        Err(e) => tracing::warn!(palette = id, error = %e, "skipping unparsable user palette"),
                    }
                }
            }
        }
        palettes
    }

    /// 按 id 取单套调色板。
    pub fn get(id: &str) -> Option<Palette> {
        Self::list().into_iter().find(|p| p.id == id)
    }

    /// 是否为内置（打包进二进制）调色板 id。
    ///
    /// 内置 id 是保留位：`save`/`delete` 均以它拒绝写入，因此用户**无法**真正覆写内置
    /// 调色板，只能新增自己的 id。前端也据此禁用内置项的删除按钮，两处语义一致。
    pub fn is_builtin(id: &str) -> bool {
        BUILTIN_PALETTES.iter().any(|(n, _)| *n == id)
    }

    /// 写入用户调色板文件；写入前完成全部字段校验。
    pub fn save(palette: &Palette) -> Result<()> {
        Self::validate_palette(palette)?;
        anyhow::ensure!(
            !Self::is_builtin(&palette.id),
            "palette id {:?} is reserved by a bundled palette",
            palette.id
        );
        let dir = Self::user_themes_dir().context("cannot locate config directory for themes")?;
        std::fs::create_dir_all(&dir).with_context(|| format!("failed to create themes dir {}", dir.display()))?;
        let path = dir.join(format!("{}.json", palette.id));
        let content = serde_json::to_string_pretty(palette).context("failed to serialize palette")?;
        std::fs::write(&path, content).with_context(|| format!("failed to write palette {}", path.display()))?;
        Ok(())
    }

    /// 删除用户调色板文件；内置调色板拒绝删除。
    pub fn delete(id: &str) -> Result<()> {
        anyhow::ensure!(is_valid_palette_id(id), "invalid palette id {:?}", id);
        anyhow::ensure!(!Self::is_builtin(id), "bundled palette {:?} cannot be deleted", id);
        let dir = Self::user_themes_dir().context("cannot locate config directory for themes")?;
        let path = dir.join(format!("{}.json", id));
        anyhow::ensure!(path.exists(), "palette {:?} not found", id);
        std::fs::remove_file(&path).map_err(|e| {
            // 已被并发删除时同样视为「不存在」，让客户端得到 404 而不是 500
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("palette {:?} not found", id)
            } else {
                anyhow::Error::new(e).context(format!("failed to delete palette {}", path.display()))
            }
        })?;
        Ok(())
    }

    /// 把主题的两套引用解析为完整调色板。
    ///
    /// 调用方须先用 `Theme::validate` 保证引用可用；缺失时回退内置默认，
    /// 避免因用户手改配置文件就让客户端拿不到主题。
    pub fn resolve(theme: &Theme) -> ResolvedTheme {
        let light = Self::get(&theme.light_palette)
            .filter(|p| p.mode == PaletteMode::Light)
            .or_else(|| Self::get("latte"))
            .expect("bundled latte palette must exist");
        let dark = Self::get(&theme.dark_palette)
            .filter(|p| p.mode == PaletteMode::Dark)
            .or_else(|| Self::get("mocha"))
            .expect("bundled mocha palette must exist");
        ResolvedTheme {
            mode: theme.mode,
            accent: if ACCENTS.contains(&theme.accent.as_str()) {
                theme.accent.clone()
            } else {
                Theme::default().accent
            },
            light,
            dark,
        }
    }
}

impl PaletteLoader {
    /// 校验调色板：id 可用于文件名、名称非空、26 个令牌均为合法十六进制色值。
    pub fn validate_palette(p: &Palette) -> Result<()> {
        anyhow::ensure!(is_valid_palette_id(&p.id), "invalid palette id {:?}", p.id);
        // 同样以 “invalid” 前缀保持与 daemon 的 400 映射一致
        anyhow::ensure!(!p.name.trim().is_empty(), "invalid palette '{}': name is empty", p.id);
        for (token, value) in p.tokens() {
            // 错误文案统一带 “invalid” 前缀：daemon 据此映射 400（见 /api/palettes 处理）
            anyhow::ensure!(
                is_valid_hex_color(value),
                "invalid palette '{}' token '{}': expect a hex color, got {:?}",
                p.id,
                token,
                value
            );
        }
        Ok(())
    }

    /// 校验主题引用：两套调色板必须存在且明暗属性能对上，强调色须在 ACCENTS 内。
    /// `palettes` 为当前可用调色板清单 (启动期读取一次)。
    pub fn validate_theme(theme: &Theme, palettes: &[Palette]) -> Result<()> {
        let light_ok = palettes
            .iter()
            .any(|p| p.id == theme.light_palette && p.mode == PaletteMode::Light);
        anyhow::ensure!(
            light_ok,
            "invalid theme.light_palette {:?}: expect a light palette id",
            theme.light_palette
        );
        let dark_ok = palettes
            .iter()
            .any(|p| p.id == theme.dark_palette && p.mode == PaletteMode::Dark);
        anyhow::ensure!(
            dark_ok,
            "invalid theme.dark_palette {:?}: expect a dark palette id",
            theme.dark_palette
        );
        anyhow::ensure!(
            ACCENTS.contains(&theme.accent.as_str()),
            "invalid theme.accent {:?}: expect one of {:?}",
            theme.accent,
            ACCENTS
        );
        Ok(())
    }
}

/// Oma 配置文件的磁盘位置。
///
/// 参照 pi 的 `settings.json` / `models.json` 分层：常规偏好与「提供商/模型清单」
/// 分文件存放，便于手改与版本管理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    /// 常规设置：默认模型/预设、推理等级、主题、服务端、MCP
    pub settings: PathBuf,
    /// 提供商与模型清单
    pub models:   PathBuf,
}

impl ConfigPaths {
    /// 标准位置 (~/.config/oma/settings.json + models.json)
    pub fn standard() -> Option<Self> {
        dirs_config_dir().map(|d| Self::in_dir(d.join("oma")))
    }

    /// 在指定目录下构造两个文件路径
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            settings: dir.join("settings.json"),
            models:   dir.join("models.json"),
        }
    }

    /// 由 settings 文件路径推导同目录的 models 文件
    pub fn from_settings_file(path: impl AsRef<Path>) -> Self {
        let settings = path.as_ref().to_path_buf();
        let models = settings
            .parent()
            .map(|p| p.join("models.json"))
            .unwrap_or_else(|| PathBuf::from("models.json"));
        Self { settings, models }
    }
}

/// Oma 根配置文件 (`~/.config/oma/settings.json` + `models.json`)
///
/// `deny_unknown_fields` 让拼错的键名与前端字段映射错误立即报错，
/// 而不是被静默忽略后「保存成功但配置没变」。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OmaConfig {
    #[serde(default = "default_model_str")]
    pub default_model:           String,
    #[serde(default = "default_agent_str")]
    pub default_agent:           String,
    /// 新会话默认推理等级（REASONING_LEVELS 之一）；不得为空，缺省为 medium
    #[serde(default = "default_reasoning_level_str")]
    pub default_reasoning_level: String,
    #[serde(default)]
    pub theme:                   Theme,
    #[serde(default)]
    pub server:                  ServerConfig,
    #[serde(default)]
    pub providers:               BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub mcp_servers:             BTreeMap<String, McpServerConfig>,
}

fn default_model_str() -> String {
    "my_anthropic/claude-3-7-sonnet".to_string()
}
fn default_agent_str() -> String {
    "build".to_string()
}
/// 默认推理等级：模型支持思考时必须落在某个等级上，故不提供「空」语义
fn default_reasoning_level_str() -> String {
    "medium".to_string()
}

impl Default for OmaConfig {
    fn default() -> Self {
        Self {
            default_model:           default_model_str(),
            default_agent:           default_agent_str(),
            default_reasoning_level: default_reasoning_level_str(),
            theme:                   Theme::default(),
            server:                  ServerConfig::default(),
            providers:               BTreeMap::new(),
            mcp_servers:             BTreeMap::new(),
        }
    }
}

impl OmaConfig {
    /// 从「settings 文件路径」加载配置；同目录 `models.json` 提供 providers。
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::load_from_paths(&ConfigPaths::from_settings_file(path))
    }

    /// 从标准位置加载；定位不到配置目录时返回默认值。
    pub fn load() -> Self {
        match ConfigPaths::standard() {
            Some(paths) => Self::load_from_paths(&paths).unwrap_or_else(|e| {
                tracing::warn!(error = %e, "failed to load config, falling back to defaults");
                Self::default()
            }),
            None => Self::default(),
        }
    }

    /// 从拆分后的 settings/models 文件加载并合并为完整配置。
    ///
    /// 以默认值为基底做浅合并：文件里缺省的键沿用默认值，未知键仍会被
    /// `deny_unknown_fields` 拒绝。
    pub fn load_from_paths(paths: &ConfigPaths) -> Result<Self> {
        let mut merged = serde_json::to_value(Self::default())?;
        if paths.settings.exists() {
            let value = read_json(&paths.settings)?;
            merge_object(&mut merged, value, &paths.settings)?;
        }
        if paths.models.exists() {
            let value = read_json(&paths.models)?;
            merge_object(&mut merged, value, &paths.models)?;
        }
        Ok(serde_json::from_value(merged)?)
    }

    /// 配置文件标准路径 (`~/.config/oma/settings.json`)
    pub fn config_path() -> Option<PathBuf> {
        ConfigPaths::standard().map(|p| p.settings)
    }

    /// 序列化并回写配置文件（前端配置修改后的持久化入口）
    pub fn save_to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        self.save_to_paths_inner(&ConfigPaths::from_settings_file(path), false)
    }

    /// 原子回写：先落同目录临时文件并 fsync，再 rename 覆盖。
    /// 避免进程中断留下半截配置，导致下次启动解析失败。
    pub fn save_to_file_atomic(&self, path: impl AsRef<Path>) -> Result<()> {
        self.save_to_paths_inner(&ConfigPaths::from_settings_file(path), true)
    }

    /// 写入拆分后的 settings.json 与 models.json（原子）。
    pub fn save_to_paths(&self, paths: &ConfigPaths) -> Result<()> {
        self.save_to_paths_inner(paths, true)
    }

    fn save_to_paths_inner(&self, paths: &ConfigPaths, atomic: bool) -> Result<()> {
        let mut settings = serde_json::to_value(self).context("Failed to serialize config")?;
        let providers = settings
            .as_object_mut()
            .and_then(|obj| obj.remove("providers"))
            .unwrap_or_else(|| serde_json::json!({}));
        let models = serde_json::json!({ "providers": providers });
        write_json(&paths.settings, &settings, atomic)?;
        write_json(&paths.models, &models, atomic)?;
        Ok(())
    }

    /// 根据 "provider/model" 字符串定位 Provider 与 Model 配置
    pub fn find_model(&self, selector: &str) -> Option<(&ProviderConfig, ModelConfig)> {
        let mut parts = selector.splitn(2, '/');
        let provider_name = parts.next()?;
        let model_id = parts.next().unwrap_or(provider_name);

        let provider = self.providers.get(provider_name)?;

        // 仅接受配置清单中显式声明的模型：未声明的 id 无法得知其上下文长度、
        // 是否支持思考等元数据，不再动态合成（调用方据此报错）
        let model_cfg = provider
            .models
            .iter()
            .find(|m| m.id == model_id)
            .map(|m| ModelConfig {
                id:               m.id.clone(),
                name:             if m.name.is_empty() {
                    m.id.clone()
                } else {
                    m.name.clone()
                },
                context_len:      m.context_len,
                capabilities:     m.capabilities.clone(),
                max_output:       m.max_output,
                // 会话等级在请求期解析后填入，模型配置不再持有默认等级
                reasoning_effort: String::new(),
                reasoning_map:    m.reasoning_map.clone(),
                headers:          m.headers.clone(),
                body:             m.body.clone(),
            })?;
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
                        id:            m.id.clone(),
                        name:          if m.name.is_empty() {
                            m.id.clone()
                        } else {
                            m.name.clone()
                        },
                        context_len:   m.context_len,
                        capabilities:  m.capabilities.clone(),
                        max_output:    m.max_output,
                        reasoning_map: m.reasoning_map.clone(),
                    })
                    .collect::<Vec<_>>();
                (p_id.clone(), models)
            })
            .collect()
    }
}

pub fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub fn dirs_config_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
}

/// oma 数据目录：`$XDG_DATA_HOME/oma`，退回 `~/.local/share/oma`。
///
/// 会话、附件、以及外部工具二进制（`bin/`）都落在这里，
/// 因此「数据放哪」只有这一处定义，守护进程与工具层共用。
pub fn dirs_data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs_home().map(|h| h.join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("oma")
}

/// 读取并解析一个 JSON 配置文件。
fn read_json(path: &Path) -> Result<serde_json::Value> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("failed to read config {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse config {}", path.display()))
}

/// 把 `src` 的顶层键并入 `dst`（浅合并）。
fn merge_object(dst: &mut serde_json::Value, src: serde_json::Value, path: &Path) -> Result<()> {
    let (Some(dst), Some(src)) = (dst.as_object_mut(), src.as_object()) else {
        anyhow::bail!("config {} must contain a JSON object", path.display());
    };
    for (k, v) in src {
        dst.insert(k.clone(), v.clone());
    }
    Ok(())
}

/// 写入 JSON 文件；`atomic` 为真时先写临时文件再 rename。
fn write_json(path: &Path, value: &serde_json::Value, atomic: bool) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("Failed to create config dir {}", parent.display()))?;
    }
    let mut content = serde_json::to_string_pretty(value).context("Failed to serialize config")?;
    content.push('\n');
    if !atomic {
        std::fs::write(path, content).with_context(|| format!("Failed to write config {}", path.display()))?;
        return Ok(());
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "settings.json".to_string());
    let tmp = path.with_file_name(format!(".{}.tmp", file_name));
    {
        let mut file =
            std::fs::File::create(&tmp).with_context(|| format!("Failed to create temp config {}", tmp.display()))?;
        std::io::Write::write_all(&mut file, content.as_bytes())
            .with_context(|| format!("Failed to write temp config {}", tmp.display()))?;
        file.sync_all()
            .with_context(|| format!("Failed to sync temp config {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("Failed to replace config {}", path.display()))?;
    Ok(())
}

// =========================================================================
// 客户端本地配置 (client.json)
// =========================================================================

/// web 客户端默认监听地址
pub const DEFAULT_WEB_HOST: &str = "127.0.0.1";

/// web 客户端默认监听端口：与 Daemon 端口分开，避免互相抢占
pub const DEFAULT_WEB_PORT: u16 = 5173;

/// web 客户端的绑定地址与端口
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebBind {
    #[serde(default = "default_web_host")]
    pub host: String,
    #[serde(default = "default_web_port")]
    pub port: u16,
}

fn default_web_host() -> String {
    DEFAULT_WEB_HOST.to_string()
}

fn default_web_port() -> u16 {
    DEFAULT_WEB_PORT
}

impl Default for WebBind {
    fn default() -> Self {
        Self {
            host: default_web_host(),
            port: default_web_port(),
        }
    }
}

/// 一条已保存的客户端连接
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// 展示名称（也是活动连接的引用键）
    pub name:  String,
    /// Daemon 访问地址
    pub url:   String,
    /// 访问 token
    #[serde(default)]
    pub token: String,
}

/// 客户端本地配置：`<配置目录>/oma/client.json`。
///
/// 与 Daemon 的 `settings.json` 分开：这里存的是「这台机器上的客户端」信息——
/// 已保存、可切换的连接列表，以及 web 客户端默认的绑定地址与端口。
/// TUI 只使用连接列表，不涉及绑定地址。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// web 客户端默认绑定地址与端口（仅 `oma web` 使用）
    #[serde(default)]
    pub web:         WebBind,
    /// 已保存的连接列表
    #[serde(default)]
    pub connections: Vec<Connection>,
    /// 当前选中的连接名；空或不存在时回落到列表首个
    #[serde(default)]
    pub active:      String,
}

impl ClientConfig {
    /// 配置文件标准路径 (~/.config/oma/client.json)
    pub fn config_path() -> Option<PathBuf> {
        dirs_config_dir().map(|d| d.join("oma").join("client.json"))
    }

    /// 从文件加载；文件不存在时返回默认配置。
    pub fn load_from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&content)?)
    }

    /// 从标准路径加载；定位不到配置目录时返回默认值。
    pub fn load() -> Self {
        match Self::config_path() {
            Some(path) => Self::load_from_file(path).unwrap_or_default(),
            None => Self::default(),
        }
    }

    /// 原子回写：先落同目录临时文件并 fsync，再 rename 覆盖。
    pub fn save_to_file_atomic(&self, path: impl AsRef<Path>) -> Result<()> {
        write_json(path.as_ref(), &serde_json::to_value(self)?, true)
    }

    /// 当前生效的连接：优先 active 指定的项，否则列表首个。
    pub fn active_connection(&self) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|c| c.name == self.active)
            .or_else(|| self.connections.first())
    }
}

// =========================================================================
// Agent 模板体系与动态环境上下文拼装
// =========================================================================

/// 内置 Agent 模板源码
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
    /// 仅正文（不含 frontmatter）：编辑器只应展示/回写正文，
    /// 否则保存时会把元信息当成提示词内容再次嵌入
    pub body:        String,
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
    /// 仅正文（不含 frontmatter）
    pub body:        String,
    /// SKILL.md 的绝对路径（模型读取技能正文的入口）
    pub path:        String,
    /// 技能目录绝对路径（技能自带的 scripts/ 等资源相对此目录解析）
    pub dir:         String,
}

/// Markdown Frontmatter 头部结构。
///
/// 全部字段可缺省：文件名为权威 id，`name` 仅是展示名（缺省回退为 id）；
/// 且**不校验未知字段**——用户目录里既有 `role: all` 这类自有约定，
/// 也可能出现未来新增的键，都不应让预设无法加载。
#[derive(Debug, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name:        Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools:       Vec<String>,
}

/// 解析后的 Frontmatter 与正文
struct ParsedTemplate {
    name:        String,
    description: String,
    tools:       Vec<String>,
    body:        String,
}

/// 解析 Markdown 的 Frontmatter 与正文；无 frontmatter 时整篇作为正文。
///
/// 解析失败（YAML 损坏）不在此处报错：调用方据 id 兜底，避免一个格式有瑕的
/// 文件让整个预设/技能不可用。
fn parse_template_parts(id: &str, raw: &str) -> ParsedTemplate {
    let fallback = ParsedTemplate {
        name:        id.to_string(),
        description: String::new(),
        tools:       Vec::new(),
        body:        raw.trim().to_string(),
    };

    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return fallback;
    };
    let Some(end_pos) = rest.find("\n---") else {
        return fallback;
    };
    let yaml_str = &rest[..end_pos];
    let body = rest[end_pos + 4..].trim();

    match serde_yaml::from_str::<Frontmatter>(yaml_str) {
        Ok(fm) => ParsedTemplate {
            name:        fm
                .name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| id.to_string()),
            description: fm.description,
            tools:       fm.tools,
            body:        body.to_string(),
        },
        Err(_) => fallback,
    }
}

/// 上下文文件的候选名（按优先级）。
///
/// `AGENTS.override.md` 用于「临时改行为而不动仓库里的 AGENTS.md」，
/// `CLAUDE.md` 是为了直接复用已有 Claude Code 项目里的说明（与 pi 一致）。
const CONTEXT_FILE_NAMES: [&str; 5] = ["AGENTS.override.md", "AGENTS.md", "AGENTS.MD", "CLAUDE.md", "CLAUDE.MD"];

/// 全局上下文文件所在目录：`<配置目录>/oma`。
///
/// 与 [`SkillLoader::agent_dir`]（技能目录）区分：这里放的是 `AGENTS.md`。
pub fn global_context_dir() -> Option<PathBuf> {
    dirs_config_dir().map(|d| d.join("oma"))
}

/// 一份上下文文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextFile {
    pub path:    PathBuf,
    pub content: String,
}

/// 在一个目录里找上下文文件：取候选名中第一个存在且为普通文件者。
///
/// 只取**一个**：同一目录同时放 `AGENTS.md` 与 `CLAUDE.md` 时应当以先命中者为准，
/// 否则两份互相矛盾的说明会一起进上下文。
fn load_context_file_from_dir(dir: &Path) -> Option<ContextFile> {
    for name in CONTEXT_FILE_NAMES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            // 不可读（权限/编码）不该让整个提示词构建失败，跳过即可
            continue;
        };
        // 去掉 BOM：它与提示词里的 `<` 相黏会被当成正文内容
        let content = raw.trim_start_matches('\u{feff}').to_string();
        return Some(ContextFile { path, content });
    }
    None
}

/// 收集上下文文件：全局（`<配置目录>/oma/AGENTS.md`）在前，
/// 其后是从文件系统根到工作区逐级向下的祖先文件（越靠近工作区越靠后）。
///
/// 一路走到文件系统根而非停在仓库根：工作区常位于仓库子目录（如 monorepo 的
/// `apps/web`），上层仓库的说明同样适用。
pub fn load_project_context_files(workspace: &Path) -> Vec<ContextFile> {
    let mut files: Vec<ContextFile> = Vec::new();
    if let Some(global) = global_context_dir().and_then(|dir| load_context_file_from_dir(&dir)) {
        files.push(global);
    }
    for file in ancestor_context_files(workspace) {
        // 与全局同路径时不重复收录（工作区就在 agent 目录之下的极端情形）
        if !files.iter().any(|f| f.path == file.path) {
            files.push(file);
        }
    }
    files
}

/// 自工作区向上逐级查找上下文文件，返回「根 → 工作区」顺序。
///
/// 与全局部分分开：全局目录来自环境变量（测试里不可控），
/// 而这里的输入完全由调用方给定。
fn ancestor_context_files(workspace: &Path) -> Vec<ContextFile> {
    let mut ancestors: Vec<ContextFile> = Vec::new();
    let mut current = workspace.to_path_buf();
    loop {
        if let Some(file) = load_context_file_from_dir(&current) {
            ancestors.push(file);
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }
    ancestors.reverse();
    ancestors
}

/// 把上下文文件渲染成一个块；无文件时返回 `None`。
fn render_project_context(files: &[ContextFile]) -> Option<String> {
    if files.is_empty() {
        return None;
    }
    let body = files
        .iter()
        .map(|f| {
            format!(
                "<project_instructions path=\"{}\">\n{}\n</project_instructions>",
                f.path.display(),
                f.content.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(format!(
        "<project_context>\nProject-specific instructions and guidelines:\n\n{body}\n</project_context>"
    ))
}

/// 解析 Markdown 的 Frontmatter 与正文
pub fn parse_markdown_template(id: &str, raw: &str) -> Result<AgentTemplate> {
    let parts = parse_template_parts(id, raw);
    Ok(AgentTemplate {
        id:                 id.to_string(),
        name:               parts.name,
        description:        parts.description,
        tools:              parts.tools,
        system_prompt_body: parts.body,
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

    /// 动态拼装注入上下文文件、实时环境块与技能目录的最终 System Prompt
    ///
    /// 基底是预设自己的正文（方案 A：模板即提示词），其后依次拼项目说明、
    /// 运行环境块与技能目录。
    pub fn build_system_prompt(template: &AgentTemplate, workspace: &Path, active_model: &str) -> String {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;

        let mut prompt = format!(
            "{}\n\n<runtime_context>\n- Workspace: {}\n- Operating System: {} ({})\n- Today: {}\n- Active Model: {}\n</runtime_context>",
            template.system_prompt_body.trim(),
            workspace.display(),
            os,
            arch,
            today,
            active_model
        );

        // 项目说明常驻上下文（与 pi 的 project_context 一致）：
        // 它是「这个仓库要遵守的约定」，不该等模型想起去 read 才生效
        if let Some(section) = render_project_context(&load_project_context_files(workspace)) {
            prompt.push_str("\n\n");
            prompt.push_str(&section);
        }

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
        let parts = parse_template_parts(id, raw);
        AgentFile {
            id:          id.to_string(),
            name:        parts.name,
            description: parts.description,
            tools:       parts.tools,
            scope:       scope.to_string(),
            content:     raw.to_string(),
            body:        parts.body,
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

/// 技能加载器：按三层根目录发现技能，每层为 `<root>/<skill-name>/SKILL.md`。
///
/// ```text
/// global   ~/.agents/skills           跨工具的用户级技能
/// agent    ~/.config/oma/skills       oma 自身的技能
/// project  <workspace>/.agents/skills 随仓库分发的技能
/// ```
///
/// 技能是可按需取用的领域知识，与 Agent 预设（角色 + 工具白名单）是两套独立机制。
/// 同名时更具体的一层覆盖更宽泛的一层：project > agent > global。
pub struct SkillLoader;

impl SkillLoader {
    /// 用户级技能根目录 (~/.agents/skills)
    pub fn global_dir() -> Option<PathBuf> {
        dirs_home().map(|h| h.join(".agents").join("skills"))
    }

    /// oma 自身的技能目录 (~/.config/oma/skills)
    pub fn agent_dir() -> Option<PathBuf> {
        dirs_config_dir().map(|d| d.join("oma").join("skills"))
    }

    /// 项目技能目录 (<workspace>/.agents/skills)
    pub fn project_dir(workspace: &Path) -> PathBuf {
        workspace.join(".agents").join("skills")
    }

    /// 三层根目录，按优先级从低到高排列
    fn roots(workspace: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
        let mut roots = Vec::new();
        if let Some(dir) = Self::global_dir() {
            roots.push((dir, "global"));
        }
        if let Some(dir) = Self::agent_dir() {
            roots.push((dir, "agent"));
        }
        if let Some(ws) = workspace {
            roots.push((Self::project_dir(ws), "project"));
        }
        roots
    }

    /// 解析单个技能目录（要求其中存在 SKILL.md）
    fn skill_from_dir(dir: &Path, scope: &str) -> Option<SkillFile> {
        let id = dir.file_name()?.to_str()?.to_string();
        let entry = dir.join("SKILL.md");
        if !entry.is_file() {
            return None;
        }
        let raw = std::fs::read_to_string(&entry).ok()?;
        let parts = parse_template_parts(&id, &raw);
        Some(SkillFile {
            id,
            name: parts.name,
            description: parts.description,
            scope: scope.to_string(),
            content: raw,
            body: parts.body,
            path: entry.to_string_lossy().to_string(),
            dir: dir.to_string_lossy().to_string(),
        })
    }

    fn skills_in_dir(root: &Path, scope: &str) -> Vec<SkillFile> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if let Some(skill) = Self::skill_from_dir(&path, scope) {
                    out.push(skill);
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// 按层合并技能：入参按优先级从低到高排列，同名时后一层覆盖前一层。
    fn collect(layers: &[(PathBuf, &'static str)]) -> Vec<SkillFile> {
        let mut by_id: std::collections::BTreeMap<String, SkillFile> = std::collections::BTreeMap::new();
        for (root, scope) in layers {
            for s in Self::skills_in_dir(root, scope) {
                by_id.insert(s.id.clone(), s);
            }
        }
        by_id.into_values().collect()
    }

    /// 列出全部技能（同 id 时 project 覆盖 agent、agent 覆盖 global）
    pub fn list_skills(workspace: Option<&Path>) -> Vec<SkillFile> {
        Self::collect(&Self::roots(workspace))
    }

    /// 读取单个技能
    pub fn read_skill(workspace: Option<&Path>, id: &str) -> Result<SkillFile> {
        Self::list_skills(workspace)
            .into_iter()
            .find(|s| s.id == id)
            .with_context(|| format!("Skill '{}' not found", id))
    }

    /// 写入技能：scope = global | agent | project，落盘为 `<root>/<id>/SKILL.md`
    pub fn write_skill(
        workspace: Option<&Path>,
        id: &str,
        scope: &str,
        name: &str,
        description: &str,
        content: &str,
    ) -> Result<PathBuf> {
        let root = Self::scope_root(workspace, scope)?;
        anyhow::ensure!(is_valid_slug(id), "Invalid skill id {:?}", id);

        let dir = root.join(id);
        std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create skill dir {}", dir.display()))?;
        let path = dir.join("SKILL.md");
        std::fs::write(&path, render_markdown(name, description, &[], content)?)
            .with_context(|| format!("Failed to write skill {}", path.display()))?;
        Ok(path)
    }

    /// 删除技能：移除整个技能目录（其中可能包含技能自带的 scripts/ 等资源）
    pub fn delete_skill(workspace: Option<&Path>, id: &str, scope: &str) -> Result<()> {
        let root = Self::scope_root(workspace, scope)?;
        // 严格校验 id 后再拼接：整个目录会被递归删除，绝不允许穿越
        anyhow::ensure!(is_valid_slug(id), "Invalid skill id {:?}", id);

        let dir = root.join(id);
        anyhow::ensure!(dir.join("SKILL.md").is_file(), "Skill {} not found", dir.display());
        std::fs::remove_dir_all(&dir).with_context(|| format!("Failed to delete skill {}", dir.display()))
    }

    /// 作用域 → 根目录
    fn scope_root(workspace: Option<&Path>, scope: &str) -> Result<PathBuf> {
        match scope {
            "global" => Self::global_dir().with_context(|| "Cannot resolve home dir for global skills"),
            "agent" => Self::agent_dir().with_context(|| "Cannot resolve config dir for agent skills"),
            "project" => {
                let ws = workspace.with_context(|| "workspace is required for project skills")?;
                Ok(Self::project_dir(ws))
            }
            other => anyhow::bail!("Invalid skill scope {:?}: expect global|agent|project", other),
        }
    }

    /// 组装注入 System Prompt 的技能目录；无技能时返回 None
    pub fn catalog(workspace: &Path) -> Option<String> {
        let skills = Self::list_skills(Some(workspace));
        if skills.is_empty() {
            return None;
        }
        let mut out = String::from(
            "<available_skills>\n以下技能是按需取用的领域知识。当任务与某个技能相关时，\n先用 read 工具读取其 SKILL.md，再按其指引执行（技能目录内的 scripts/ 等\n资源用相对路径解析）；不相关时无需读取。\n",
        );
        for s in &skills {
            let summary = if s.description.is_empty() {
                &s.name
            } else {
                &s.description
            };
            out.push_str(&format!("- {}: {} (read: {})\n", s.id, summary, s.path));
        }
        out.push_str("</available_skills>");
        Some(out)
    }
}

/// 内置 Agent 预设清单（id, 模板源码）
pub const BUNDLED_AGENTS: [(&str, &str); 4] = [
    ("plan", TEMPLATE_PLAN),
    ("explore", TEMPLATE_EXPLORE),
    ("review", TEMPLATE_REVIEW),
    ("build", TEMPLATE_BUILD),
];

/// 预设/技能标识符：字母数字与 - _，非空。
///
/// id 直接充当文件名（`<id>.md`、`<id>/SKILL.md`），故按字母表放行：
/// 用 `is_alphanumeric`（Unicode 感知）而非 `is_ascii_alphanumeric`，
/// 中文等非 ASCII 名称才能作为合法 id——用户手写的 `猫娘.md` 即属此类。
/// 路径分隔符、`.`、空白与控制字符都不属于字母数字，天然被挡在路径外。
fn is_valid_slug(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
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
    use oma_contract::{ModelCapability, default_model_capabilities};

    use super::*;

    #[test]
    fn test_bundled_palettes_are_valid() {
        let palettes = PaletteLoader::builtin_all();
        assert_eq!(palettes.len(), 4, "four bundled palettes expected");
        for p in &palettes {
            PaletteLoader::validate_palette(p).unwrap_or_else(|e| panic!("bundled palette {} invalid: {e}", p.id));
            assert_eq!(p.tokens().len(), PALETTE_TOKENS.len());
        }
        // 内置只剩 Catppuccin 四套：浅色唯 `latte`，深色为 `frappe` / `macchiato` / `mocha`；
        // 默认主题走 `mocha` / `latte`（同一套语义色的深色与浅色版本）
        let light: Vec<&str> = palettes
            .iter()
            .filter(|p| p.mode == PaletteMode::Light)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(light, vec!["latte"]);

        let dark: Vec<&str> = palettes
            .iter()
            .filter(|p| p.mode == PaletteMode::Dark)
            .map(|p| p.id.as_str())
            .collect();
        assert_eq!(dark, vec!["frappe", "macchiato", "mocha"]);
    }

    #[test]
    fn test_theme_validation() {
        let palettes = PaletteLoader::builtin_all();

        // 默认主题引用的均为内置调色板
        let default_theme = Theme::default();
        assert!(PaletteLoader::validate_theme(&default_theme, &palettes).is_ok());
        // 缺省即 `mocha` / `latte`：新装配置不写主题字段时客户端拿到的就是这两套
        assert_eq!(default_theme.dark_palette, "mocha");
        assert_eq!(default_theme.light_palette, "latte");

        // 浅色引用深色调色板非法
        let wrong_mode = Theme {
            light_palette: "mocha".into(),
            ..Theme::default()
        };
        assert!(PaletteLoader::validate_theme(&wrong_mode, &palettes).is_err());

        let unknown = Theme {
            dark_palette: "nord-dark".into(),
            ..Theme::default()
        };
        assert!(PaletteLoader::validate_theme(&unknown, &palettes).is_err());

        let bad_accent = Theme {
            accent: "teal-ish".into(),
            ..Theme::default()
        };
        assert!(PaletteLoader::validate_theme(&bad_accent, &palettes).is_err());
    }

    #[test]
    fn test_custom_palette_validation() {
        let mocha = PaletteLoader::get("mocha").unwrap();

        // 改名不影响引用：id 稳定，name 仅展示
        let renamed = Palette {
            name: "My Mocha".into(),
            ..mocha.clone()
        };
        PaletteLoader::validate_palette(&renamed).unwrap();

        // 非法 id / 非十六进制色值 / 空名称均被拒绝
        let bad_id = Palette {
            id: "../escape".into(),
            ..mocha.clone()
        };
        assert!(PaletteLoader::validate_palette(&bad_id).is_err());

        let bad_color = Palette {
            base: "not-a-color".into(),
            ..mocha.clone()
        };
        assert!(PaletteLoader::validate_palette(&bad_color).is_err());

        let empty_name = Palette {
            name: "  ".into(),
            ..mocha.clone()
        };
        assert!(PaletteLoader::validate_palette(&empty_name).is_err());

        // 内置调色板 id 为保留位，不允许落盘覆盖
        let mut shadow = mocha.clone();
        shadow.name = "Shadow".into();
        let err = PaletteLoader::save(&shadow).unwrap_err().to_string();
        assert!(err.contains("reserved"), "unexpected error: {err}");
    }

    #[test]
    fn test_resolve_theme_falls_back_to_bundled() {
        // 引用已被删除的调色板时仍能解析出可用主题，而不是把界面搞成空白
        let dangling = Theme {
            dark_palette: "gone".into(),
            light_palette: "also-gone".into(),
            ..Theme::default()
        };
        let resolved = PaletteLoader::resolve(&dangling);
        assert_eq!(resolved.dark.id, "mocha");
        assert_eq!(resolved.light.id, "latte");
        assert_eq!(resolved.accent, "blue");
    }

    #[test]
    fn test_load_bundled_agents() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path();

        // 缺省预设是 `build`：它不声明工具清单，空清单在工具注册表里等于「不限制」，
        // 因此新装即拥有全部工具，不需要先写配置文件
        let build = AgentLoader::load_agent("build", ws).unwrap();
        assert_eq!(build.id, "build");
        assert_eq!(build.name, "Build");
        assert!(build.tools.is_empty(), "build 预设不应限制工具: {:?}", build.tools);

        // 只读预设仍须声明白名单；命令工具在内核里叫 `shell`
        let explore = AgentLoader::load_agent("explore", ws).unwrap();
        assert!(explore.tools.contains(&"read".to_string()));
        assert!(explore.tools.contains(&"shell".to_string()));

        // `task` 已并入 `build`，内置清单不再包含它
        let bundled_ids: Vec<&str> = BUNDLED_AGENTS.iter().map(|(id, _)| *id).collect();
        assert_eq!(bundled_ids, vec!["plan", "explore", "review", "build"]);

        // 已从内核移除的工具不应再出现在任何预设里（白名单会空转，看起来像坏了）
        for bundled in AgentLoader::list_agent_files(None) {
            for gone in ["bash", "task", "ask"] {
                assert!(
                    !bundled.tools.contains(&gone.to_string()),
                    "预设 {} 仍声明了已移除的工具 {}",
                    bundled.id,
                    gone
                );
            }
        }

        let prompt = AgentLoader::build_system_prompt(&build, ws, "my_anthropic/claude-3-7");
        assert!(prompt.contains("<runtime_context>"));
        assert!(prompt.contains("Active Model: my_anthropic/claude-3-7"));
    }

    /// 同一目录里多个候选文件只取一个，且 `override` 优先。
    #[test]
    fn test_context_file_candidate_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CLAUDE.md"), "from claude").unwrap();
        assert_eq!(load_context_file_from_dir(tmp.path()).unwrap().content, "from claude");

        std::fs::write(tmp.path().join("AGENTS.md"), "from agents").unwrap();
        assert_eq!(load_context_file_from_dir(tmp.path()).unwrap().content, "from agents");

        std::fs::write(tmp.path().join("AGENTS.override.md"), "from override").unwrap();
        assert_eq!(load_context_file_from_dir(tmp.path()).unwrap().content, "from override");
    }

    /// BOM 不该黏在正文开头（它会跟标签搅在一起）。
    #[test]
    fn test_context_file_strips_bom() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "\u{feff}hello").unwrap();
        assert_eq!(load_context_file_from_dir(tmp.path()).unwrap().content, "hello");
    }

    /// 祖先文件按「根 → 工作区」排序：越靠近工作区的越靠后（越具体）。
    #[test]
    fn test_ancestor_context_files_are_root_to_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = tmp.path().join("packages/app");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "outer").unwrap();
        std::fs::write(tmp.path().join("packages/AGENTS.md"), "middle").unwrap();
        std::fs::write(inner.join("CLAUDE.md"), "inner").unwrap();

        let files = ancestor_context_files(&inner);
        let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
        // 末尾三份一定是「外 → 中 → 内」，前面可能还有测试环境里真实存在的其它祖先文件
        assert!(
            contents.len() >= 3,
            "expected at least our three files, got {contents:?}"
        );
        assert_eq!(contents[contents.len() - 3..], ["outer", "middle", "inner"]);
    }

    /// 渲染出的块带路径，便于模型/用户区分来自哪个文件。
    #[test]
    fn test_project_context_renders_paths() {
        let files = vec![ContextFile {
            path:    PathBuf::from("/repo/AGENTS.md"),
            content: "run cargo test".into(),
        }];
        let section = render_project_context(&files).unwrap();
        assert!(section.contains("<project_context>"));
        assert!(section.contains("Project-specific instructions and guidelines:"));
        assert!(section.contains("<project_instructions path=\"/repo/AGENTS.md\">"));
        assert!(section.contains("run cargo test"));

        assert!(render_project_context(&[]).is_none());
    }

    /// 端到端：工作区里的 AGENTS.md 会进系统提示词，别的工作区不会。
    #[test]
    fn test_system_prompt_includes_project_context() {
        let template = AgentLoader::load_agent("build", Path::new(".")).unwrap();

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("AGENTS.md"), "MARKER-ONLY-IN-THIS-WORKSPACE").unwrap();
        let prompt = AgentLoader::build_system_prompt(&template, tmp.path(), "p/m");
        assert!(prompt.contains("<project_context>"));
        assert!(prompt.contains("MARKER-ONLY-IN-THIS-WORKSPACE"));
        // 顺序：预设正文 → 环境块 → 项目说明 → 技能目录
        let ctx_at = prompt.find("<project_context>").unwrap();
        assert!(prompt.find("</runtime_context>").unwrap() < ctx_at);
        // 基底是预设正文（方案 A：模板即提示词）
        assert!(prompt.starts_with(&template.system_prompt_body.trim()[..40]));

        let clean = tempfile::tempdir().unwrap();
        let other = AgentLoader::build_system_prompt(&template, clean.path(), "p/m");
        assert!(!other.contains("MARKER-ONLY-IN-THIS-WORKSPACE"));
    }

    #[test]
    fn test_model_headers_and_body_reach_model_config() {
        let config: OmaConfig = serde_json::from_str(
            r#"{
              "providers": {
                "p1": {
                  "api_type": "completion",
                  "base_url": "http://localhost/v1",
                  "api_key": "k",
                  "headers": { "X-Provider": "p" },
                  "body": { "temperature": 0.2 },
                  "models": [
                    {
                      "id": "m1",
                      "headers": { "X-Model": "m", "X-Provider": "override" },
                      "body": { "temperature": 0.9, "top_p": 0.5 }
                    },
                    { "id": "m2" }
                  ]
                }
              }
            }"#,
        )
        .unwrap();

        // 模型级配置必须落到 ModelConfig，否则会静默丢失（请求仍按 Provider 级发出）
        let (_, m1) = config.find_model("p1/m1").unwrap();
        assert_eq!(m1.headers.get("X-Model").map(String::as_str), Some("m"));
        assert_eq!(m1.body["temperature"], 0.9);
        assert_eq!(m1.body["top_p"], 0.5);

        // 未配置的模型不受其他模型配置影响
        let (_, m2) = config.find_model("p1/m2").unwrap();
        assert!(m2.headers.is_empty());
        assert_eq!(m2.body, serde_json::json!({}));
    }

    #[test]
    fn test_model_entry_omits_empty_override_fields() {
        // 空覆写不落盘：模型条目多时每项都带空 body/headers 会淹没真正的配置
        let entry: ModelEntry = serde_json::from_str(r#"{"id": "m"}"#).unwrap();
        let out = serde_json::to_string(&entry).unwrap();
        assert!(!out.contains("body"), "empty body must be skipped: {out}");
        assert!(!out.contains("headers"), "empty headers must be skipped: {out}");

        let rich: ModelEntry =
            serde_json::from_str(r#"{"id": "m", "headers": {"X-A": "1"}, "body": {"temperature": 0.7}}"#).unwrap();
        let out = serde_json::to_string(&rich).unwrap();
        assert!(out.contains("X-A"));
        assert!(out.contains("temperature"));
    }

    #[test]
    fn test_parse_config_json() {
        let config: OmaConfig = serde_json::from_str(
            r#"{
              "default_model": "deepseek/deepseek-chat",
              "default_agent": "build",
              "server": { "host": "0.0.0.0", "port": 8080 },
              "providers": {
                "deepseek": {
                  "api_type": "completion",
                  "base_url": "https://api.deepseek.com/v1",
                  "api_key": "env:DEEPSEEK_KEY"
                }
              }
            }"#,
        )
        .unwrap();
        assert_eq!(config.default_model, "deepseek/deepseek-chat");
        assert_eq!(config.default_agent, "build");
        assert_eq!(config.server.host, "0.0.0.0");
        assert_eq!(config.server.port, 8080);
        assert!(config.providers.contains_key("deepseek"));
    }

    /// 缺省值：服务端绑回环 17431，预设为 build——新装不写任何字段即可用。
    #[test]
    fn test_config_defaults() {
        let config = OmaConfig::default();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 17431);
        assert_eq!(config.default_agent, "build");

        // `server` 段落可只写一半：缺的字段各自回落默认值
        let partial: OmaConfig = serde_json::from_str(r#"{"server": {"port": 8080}}"#).unwrap();
        assert_eq!(partial.server.host, "127.0.0.1");
        assert_eq!(partial.server.port, 8080);

        // 客户端默认连接目标与默认绑定同源，端口只在一处定义
        assert_eq!(default_server_addr(), "127.0.0.1:17431");
    }

    #[test]
    fn test_configured_models_require_declaration() {
        let config: OmaConfig = serde_json::from_str(
            r#"{
              "providers": {
                "p1": {
                  "api_type": "completion",
                  "base_url": "http://localhost/v1",
                  "api_key": "k",
                  "models": [
                    {
                      "id": "m1",
                      "name": "Model One",
                      "context_len": 1048576,
                      "capabilities": ["text_input", "image_input"]
                    },
                    { "id": "m2" }
                  ]
                },
                "p2": {
                  "api_type": "anthropic",
                  "base_url": "http://localhost/v1",
                  "api_key": "k"
                }
              }
            }"#,
        )
        .unwrap();

        // 配置清单内的模型: 精确元数据
        let (_, m1) = config.find_model("p1/m1").unwrap();
        assert_eq!(m1.name, "Model One");
        assert_eq!(m1.context_len, 1_048_576);
        assert!(m1.has(ModelCapability::ImageInput));
        assert!(m1.has(ModelCapability::TextInput));
        assert!(!m1.has(ModelCapability::Thinking));
        // 未声明 capabilities 时仅文本输入与输出
        let (_, m2) = config.find_model("p1/m2").unwrap();
        assert_eq!(m2.name, "m2");
        assert_eq!(m2.context_len, 128_000);
        assert_eq!(m2.capabilities, default_model_capabilities());
        // 未声明的 model id 不再动态合成：元数据无从得知，返回 None 由调用方报错
        assert!(config.find_model("p1/not-listed").is_none());
        // 无清单 provider 的任何模型都不可用
        assert!(config.find_model("p2/anything").is_none());

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
        let path = tmp.path().join("oma").join("settings.json");

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

        // 拆分落盘：常规设置在 settings.json，提供商在 models.json
        assert!(path.exists());
        let models_path = tmp.path().join("oma").join("models.json");
        assert!(models_path.exists());
        let settings_raw = std::fs::read_to_string(&path).unwrap();
        assert!(!settings_raw.contains("providers"), "providers belongs in models.json");

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
        assert!(AgentLoader::write_agent_file(Some(&ws), "build", "global", "X", "", &[], "body").is_err());

        // 删除后回落到内置兜底
        AgentLoader::delete_agent_file(Some(&ws), "my-preset", "project").unwrap();
        assert!(AgentLoader::read_agent_file(Some(&ws), "my-preset").is_err());
        // 内置兜底仍然可用
        assert!(AgentLoader::read_agent_file(Some(&ws), "build").is_ok());
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

        SkillLoader::write_skill(Some(&ws), "build", "project", "同名技能", "与预设同名", "技能正文").unwrap();
        let skills = project_skills(&ws);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].id, "build");
        assert_eq!(skills[0].scope, "project");

        // 同名互不覆盖：预设仍是内置只读模板，工具清单也只来自预设（build 不限制工具），
        // 技能里的任何内容都不会改写它
        let preset = AgentLoader::read_agent_file(Some(&ws), "build").unwrap();
        assert_eq!(preset.scope, "bundled");
        assert_ne!(preset.name, "同名技能");
        assert!(preset.tools.is_empty(), "build 预设不声明工具清单");

        // 技能存放在独立的 skills 目录下，且为 <name>/SKILL.md 布局
        let skill_dir = SkillLoader::project_dir(&ws);
        let preset_dir = AgentLoader::project_agents_dir(&ws);
        assert_ne!(skill_dir, preset_dir);
        assert!(skill_dir.join("build").join("SKILL.md").exists());
        assert!(!preset_dir.join("build.md").exists());
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
        assert!(skill.path.ends_with("tricky/SKILL.md"));

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

        let preset = AgentLoader::load_agent("build", &ws).unwrap();
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

    /// 三层技能根目录各自解析到约定路径，且与预设目录分离。
    #[test]
    fn test_skill_dirs_resolve() {
        let ws = std::path::Path::new("/tmp/ws");
        assert!(
            SkillLoader::global_dir()
                .unwrap()
                .ends_with(".agents/skills"),
            "global layer is the cross-tool ~/.agents/skills"
        );
        assert!(SkillLoader::agent_dir().unwrap().ends_with("oma/skills"));
        assert!(
            SkillLoader::project_dir(ws).ends_with(".agents/skills"),
            "project layer is <workspace>/.agents/skills"
        );
        assert!(AgentLoader::project_agents_dir(ws).ends_with(".oma/agents"));
    }

    /// 预设/技能文件缺 `name`、或带有未知 frontmatter 键时仍须可用。
    ///
    /// 用户目录里存在这类文件（如 `description` + `role: all`，名称即文件名），
    /// 此前会因 name 必填 + YAML 严格解析而整条报错。
    #[test]
    fn test_frontmatter_tolerates_missing_name_and_extra_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        let dir = AgentLoader::project_agents_dir(&ws);
        std::fs::create_dir_all(&dir).unwrap();

        // 缺 name，且带未知键 role
        std::fs::write(
            dir.join("minimal.md"),
            "---\ndescription: 极简的软件工程助手\nrole: all\n---\n你是一位乐于助人的软件工程师助手。",
        )
        .unwrap();
        // 完全没有 frontmatter
        std::fs::write(dir.join("bare.md"), "纯正文，没有元信息。").unwrap();

        let minimal = AgentLoader::read_agent_file(Some(&ws), "minimal").unwrap();
        assert_eq!(minimal.name, "minimal", "name 缺省时回退为文件名");
        assert_eq!(minimal.description, "极简的软件工程助手");
        assert_eq!(minimal.body, "你是一位乐于助人的软件工程师助手。");
        assert!(minimal.tools.is_empty());

        let bare = AgentLoader::read_agent_file(Some(&ws), "bare").unwrap();
        assert_eq!(bare.name, "bare");
        assert_eq!(bare.body, "纯正文，没有元信息。");

        // 且能真正加载为可用模板（此前这里会报 missing field `name`）
        let template = AgentLoader::load_agent("minimal", &ws).unwrap();
        assert_eq!(template.system_prompt_body, "你是一位乐于助人的软件工程师助手。");
    }

    /// `body` 只含正文：编辑器回写正文，元信息由服务端按字段重新渲染，
    /// 因此保存后 frontmatter 不会作为提示词内容被二次嵌入。
    #[test]
    fn test_body_excludes_frontmatter_and_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();

        let body = "正文第一行\n\n正文第二行";
        AgentLoader::write_agent_file(
            Some(&ws),
            "p",
            "project",
            "Name",
            "Desc: 含冒号",
            &["read".to_string()],
            body,
        )
        .unwrap();
        let read = AgentLoader::read_agent_file(Some(&ws), "p").unwrap();
        assert_eq!(read.body, body, "body 必须是纯正文");
        assert!(read.content.starts_with("---"), "content 保留完整原文");
        assert!(!read.body.contains("---"), "正文不得包含 frontmatter 分隔符");

        // 以 body 作为新内容再次保存，frontmatter 不应累积
        AgentLoader::write_agent_file(Some(&ws), "p", "project", "Name", "Desc: 含冒号", &[], &read.body).unwrap();
        let again = AgentLoader::read_agent_file(Some(&ws), "p").unwrap();
        assert_eq!(again.body, body);
        assert_eq!(again.content.matches("---").count(), 2, "frontmatter 只应出现一对");
    }

    /// 三层优先级：同名技能只保留最具体的一层（project > agent > global）。
    ///
    /// 用合成目录验证合并逻辑，不读写真实用户目录。
    #[test]
    fn test_skill_layer_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let mk = |layer: &str, id: &str, name: &str| {
            let dir = tmp.path().join(layer).join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {}\ndescription: from {}\n---\n\nbody", name, layer),
            )
            .unwrap();
            dir
        };

        mk("global", "shared", "from-global");
        mk("agent", "shared", "from-agent");
        mk("project", "shared", "from-project");
        mk("global", "only-global", "g");

        // 三层同名 → 只留 project
        let layers = vec![
            (tmp.path().join("global"), "global"),
            (tmp.path().join("agent"), "agent"),
            (tmp.path().join("project"), "project"),
        ];
        let merged = SkillLoader::collect(&layers);
        let shared: Vec<&SkillFile> = merged.iter().filter(|s| s.id == "shared").collect();
        assert_eq!(shared.len(), 1, "same id must collapse to one entry");
        assert_eq!(shared[0].scope, "project");
        assert_eq!(shared[0].name, "from-project");
        assert!(merged.iter().any(|s| s.id == "only-global"));

        // 缺少更具体的一层时回落到 agent
        let two = vec![
            (tmp.path().join("global"), "global"),
            (tmp.path().join("agent"), "agent"),
        ];
        let merged = SkillLoader::collect(&two);
        assert_eq!(merged.iter().find(|s| s.id == "shared").unwrap().scope, "agent");

        // 只有 global 时用 global
        let one = vec![(tmp.path().join("global"), "global")];
        assert_eq!(
            SkillLoader::collect(&one)
                .iter()
                .find(|s| s.id == "shared")
                .unwrap()
                .scope,
            "global"
        );

        // 缺少 SKILL.md 的目录不算技能
        std::fs::create_dir_all(tmp.path().join("project").join("not-a-skill")).unwrap();
        assert!(
            SkillLoader::collect(&layers)
                .iter()
                .all(|s| s.id != "not-a-skill")
        );
    }

    /// 技能以 `<name>/SKILL.md` 落盘，且技能目录内的资源随目录一并删除。
    #[test]
    fn test_skill_dir_layout_and_scripts_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();

        let path = SkillLoader::write_skill(Some(&ws), "with-scripts", "project", "S", "d", "body").unwrap();
        assert!(
            path.ends_with("with-scripts/SKILL.md"),
            "unexpected layout: {}",
            path.display()
        );

        // 技能自带资源
        let scripts = SkillLoader::project_dir(&ws)
            .join("with-scripts")
            .join("scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("run.py"), "print(1)").unwrap();

        let skill = SkillLoader::read_skill(Some(&ws), "with-scripts").unwrap();
        assert!(skill.path.ends_with("with-scripts/SKILL.md"));
        assert!(
            skill.dir.ends_with("with-scripts"),
            "dir must expose the skill root: {}",
            skill.dir
        );

        // 删除技能应连同其资源目录一起移除
        SkillLoader::delete_skill(Some(&ws), "with-scripts", "project").unwrap();
        assert!(!SkillLoader::project_dir(&ws).join("with-scripts").exists());
        assert!(SkillLoader::read_skill(Some(&ws), "with-scripts").is_err());
    }

    /// 非 ASCII 名称必须是合法 id：用户手写的 `猫娘.md` / `极简.md` 直接充当 id，
    /// 此前仅放行 ASCII，保存时报 "Invalid preset id"。
    #[test]
    fn test_non_ascii_id_allowed() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();

        for id in ["猫娘", "极简", "预设-2", "café", "тест"] {
            AgentLoader::write_agent_file(Some(&ws), id, "project", id, "", &[], "正文")
                .unwrap_or_else(|e| panic!("id {id:?} 应当合法: {e}"));
            let got = AgentLoader::read_agent_file(Some(&ws), id).unwrap();
            assert_eq!(got.body, "正文");
            // 生成的文件名就是 `<id>.md`（项目层为 <workspace>/.oma/agents）
            assert!(
                AgentLoader::project_agents_dir(&ws)
                    .join(format!("{id}.md"))
                    .exists()
            );
            AgentLoader::delete_agent_file(Some(&ws), id, "project")
                .unwrap_or_else(|e| panic!("id {id:?} 应当可删: {e}"));
        }

        // 技能同理（`<id>/SKILL.md`）
        SkillLoader::write_skill(Some(&ws), "猫娘技能", "project", "猫娘技能", "描述", "正文").unwrap();
        assert!(
            SkillLoader::project_dir(&ws)
                .join("猫娘技能")
                .join("SKILL.md")
                .exists()
        );
        SkillLoader::delete_skill(Some(&ws), "猫娘技能", "project").unwrap();
    }

    /// 放行非 ASCII 不得顺带放开路径穿越：分隔符与 `.` 仍被拒。
    #[test]
    fn test_non_ascii_widening_keeps_paths_safe() {
        for evil in ["../x", "a/b", "a\\b", "..", ".", "", "a b", "a\u{0}b", "x.md"] {
            assert!(!is_valid_slug(evil), "{evil:?} 必须仍被拒绝（id 会拼进文件路径）");
        }
    }

    /// 删除技能的 id 会被拼进路径：穿越型 id 必须被拒且不得删除任何东西。
    #[test]
    fn test_skill_id_traversal_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        let target = SkillLoader::project_dir(&ws).join("victim");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), "---\nname: v\n---\n").unwrap();
        std::fs::write(tmp.path().join("keep.txt"), "keep").unwrap();

        for evil in ["../../etc", "..", "a/b", "", "a b"] {
            assert!(
                SkillLoader::delete_skill(Some(&ws), evil, "project").is_err(),
                "delete_skill must reject {:?}",
                evil
            );
        }
        assert!(target.exists(), "victim skill dir must be untouched");
        assert!(tmp.path().join("keep.txt").exists());
    }

    /// 作用域取值必须受限，避免把技能写到意料之外的路径。
    #[test]
    fn test_skill_scope_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = tmp.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();
        assert!(SkillLoader::write_skill(Some(&ws), "x", "bundled", "n", "", "b").is_err());
        assert!(SkillLoader::write_skill(Some(&ws), "x", "../escape", "n", "", "b").is_err());
        // project 作用域缺 workspace 必须报错
        assert!(SkillLoader::write_skill(None, "x", "project", "n", "", "b").is_err());
    }

    /// 配置键拼错必须报错，而不是被静默忽略。
    #[test]
    fn test_unknown_config_field_is_rejected() {
        let err = serde_json::from_str::<OmaConfig>(r#"{"default_model": "p/m", "model": "p/other"}"#);
        assert!(err.is_err(), "unknown key must not be silently ignored");
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("model"), "error should name the offending key: {}", msg);

        // 合法配置不受影响
        assert!(serde_json::from_str::<OmaConfig>(r#"{"default_model": "p/m"}"#).is_ok());
    }

    /// 未知键在任意层级都必须报错：旧版残留键名被静默忽略时，
    /// 用户会以为设置已生效（历史上 `theme.dark_flavor` 即如此）。
    #[test]
    fn test_unknown_config_field_is_rejected_at_every_level() {
        let cases = [
            ("theme", r#"{"theme": {"dark_flavor": "mocha"}}"#),
            // `listen_addr` 已拆成 host + port，不做兼容：旧键必须报错，
            // 否则用户以为端口改了、实际还绑在旧地址上
            ("server", r#"{"server": {"listen_addr": "0.0.0.0:17431"}}"#),
            (
                "provider",
                r#"{"providers": {"p": {"api_type": "completion", "base_url": "http://x/v1", "api_key": "k", "modles": []}}}"#,
            ),
            (
                "model",
                r#"{"providers": {"p": {"api_type": "completion", "base_url": "http://x/v1", "api_key": "k", "models": [{"id": "m", "ctx": 100}]}}}"#,
            ),
            (
                "mcp server",
                r#"{"mcp_servers": {"s": {"type": "local", "command": "x", "timeout": 1}}}"#,
            ),
        ];

        for (level, json_str) in cases {
            assert!(
                serde_json::from_str::<OmaConfig>(json_str).is_err(),
                "unknown {level} field must not be silently ignored"
            );
        }
    }

    /// client.json：缺省字段可省略、写读往返保持一致，active 无匹配时回落首项。
    #[test]
    fn test_client_config_roundtrip_and_active_fallback() {
        // 仅写连接列表时，web 段落应使用默认绑定地址与端口
        let minimal: ClientConfig = serde_json::from_str(
            r#"
{
  "active": "b",
  "connections": [
    {"name": "a", "url": "http://127.0.0.1:17431", "token": "admin"},
    {"name": "b", "url": "http://10.0.0.5:17431"}
  ]
}
"#,
        )
        .unwrap();
        assert_eq!(minimal.web.host, DEFAULT_WEB_HOST);
        assert_eq!(minimal.web.port, DEFAULT_WEB_PORT);
        assert_eq!(minimal.active_connection().unwrap().name, "b");

        // active 不匹配时回落列表首个
        let stale = ClientConfig {
            active: "missing".into(),
            ..minimal.clone()
        };
        assert_eq!(stale.active_connection().unwrap().name, "a");

        // 写盘后重新加载，内容一致（含 web 绑定地址与端口）
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("oma").join("client.json");
        stale.save_to_file_atomic(&path).unwrap();
        let loaded = ClientConfig::load_from_file(&path).unwrap();
        assert_eq!(loaded.connections, minimal.connections);
        assert_eq!(loaded.active, "missing");
    }
}
