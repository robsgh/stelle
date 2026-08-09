use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use mlua::Lua;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardConfig {
    pub dashboard: DashboardSettings,
    pub widgets: Vec<WidgetConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DashboardSettings {
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default = "default_accent")]
    pub accent: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    #[default]
    System,
}

fn default_accent() -> String {
    "#8b5cf6".into()
}
fn default_columns() -> u8 {
    4
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum WidgetConfig {
    Link(LinkWidget),
    Lua {
        id: String,
        script: String,
        #[serde(default)]
        settings: BTreeMap<String, Value>,
        #[serde(default)]
        network_allow: Vec<String>,
        #[serde(default = "default_columns")]
        columns: u8,
    },
}

impl WidgetConfig {
    fn id(&self) -> &str {
        match self {
            Self::Link(widget) => &widget.id,
            Self::Lua { id, .. } => id,
        }
    }
    fn columns(&self) -> u8 {
        match self {
            Self::Link(widget) => widget.columns,
            Self::Lua { columns, .. } => *columns,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadedConfig {
    #[serde(flatten)]
    pub dashboard: DashboardSettings,
    pub widgets: Vec<LoadedWidget>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LoadedWidget {
    Link(LinkWidget),
    Lua(LuaWidget),
}

impl LoadedWidget {
    pub fn id(&self) -> &str {
        match self {
            Self::Link(w) => &w.id,
            Self::Lua(w) => &w.id,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LinkWidget {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub url: String,
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default = "default_columns")]
    pub columns: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct LuaWidget {
    pub id: String,
    #[serde(skip)]
    pub source: String,
    #[serde(skip)]
    pub settings: BTreeMap<String, Value>,
    #[serde(skip)]
    pub network_allow: Vec<Url>,
    pub columns: u8,
}

pub fn load(path: &Path) -> Result<LoadedConfig> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("could not read configuration {}", path.display()))?;
    let config: DashboardConfig = serde_yaml::from_str(&raw)
        .with_context(|| format!("invalid configuration {}", path.display()))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    validate_and_load(config, base)
}

fn validate_and_load(config: DashboardConfig, base: &Path) -> Result<LoadedConfig> {
    if config.dashboard.title.trim().is_empty() {
        bail!("dashboard title cannot be empty");
    }
    let mut ids = std::collections::HashSet::new();
    let mut loaded = Vec::with_capacity(config.widgets.len());

    for widget in config.widgets {
        if widget.id().trim().is_empty() {
            bail!("widget id cannot be empty");
        }
        if !ids.insert(widget.id().to_owned()) {
            bail!("duplicate widget id: {}", widget.id());
        }
        if !(1..=12).contains(&widget.columns()) {
            bail!("widget {} must span between 1 and 12 columns", widget.id());
        }

        match widget {
            WidgetConfig::Link(widget) => {
                validate_http_url(&widget.url)
                    .with_context(|| format!("invalid link URL for {}", widget.id))?;
                loaded.push(LoadedWidget::Link(widget));
            }
            WidgetConfig::Lua {
                id,
                script,
                settings,
                network_allow,
                columns,
            } => {
                if Path::new(&script).is_absolute()
                    || Path::new(&script)
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
                {
                    bail!("widget {id} script must be a relative path without '..'");
                }
                let network_allow = network_allow
                    .into_iter()
                    .map(|origin| {
                        validate_origin(&origin)
                            .with_context(|| format!("invalid network origin for {id}"))
                    })
                    .collect::<Result<_>>()?;
                let script_path: PathBuf = base.join(&script);
                let source = fs::read_to_string(&script_path).with_context(|| {
                    format!(
                        "could not read script {} for widget {id}",
                        script_path.display()
                    )
                })?;
                Lua::new()
                    .load(&source)
                    .set_name(&script)
                    .into_function()
                    .with_context(|| format!("could not compile script for widget {id}"))?;
                loaded.push(LoadedWidget::Lua(LuaWidget {
                    id,
                    source,
                    settings,
                    network_allow,
                    columns,
                }));
            }
        }
    }
    Ok(LoadedConfig {
        dashboard: config.dashboard,
        widgets: loaded,
    })
}

pub fn validate_http_url(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("only absolute HTTP(S) URLs are supported");
    }
    Ok(url)
}

pub fn validate_origin(value: &str) -> Result<Url> {
    let url = validate_http_url(value)?;
    if url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("network allow entries must be origins such as https://api.example.com");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_links_with_nonstandard_ports() {
        let url = validate_http_url("https://mini.i.schminfra.net:8006").unwrap();
        assert_eq!(url.port(), Some(8006));
    }

    #[test]
    fn rejects_origin_paths() {
        assert!(validate_origin("https://example.com/api").is_err());
    }

    #[test]
    fn bundled_configuration_loads() {
        let config = load(Path::new("config/dashboard.yaml")).unwrap();
        assert_eq!(config.widgets.len(), 3);
        assert!(
            config
                .widgets
                .iter()
                .any(|widget| widget.id() == "stelle-github")
        );
        assert!(config.widgets.iter().any(|widget| widget.id() == "youtube"));
        assert!(config.widgets.iter().any(|widget| widget.id() == "proxmox"));
    }
}
