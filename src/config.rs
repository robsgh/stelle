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

const DEFAULT_CACHE_TTL: u64 = 300;
const MIN_CACHE_TTL: u64 = 10;
const MAX_CACHE_TTL: u64 = 86_400;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Dashboard<W> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub theme: Theme,
    #[serde(default = "default_accent")]
    pub accent: String,
    pub widgets: Vec<W>,
}

type DashboardConfig = Dashboard<WidgetConfig>;
pub type LoadedConfig = Dashboard<LoadedWidget>;

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

fn default_cache_ttl() -> u64 {
    DEFAULT_CACHE_TTL
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase", deny_unknown_fields)]
pub enum WidgetConfig {
    Link(LinkConfig),
    Lua {
        id: String,
        script: String,
        #[serde(default = "default_cache_ttl")]
        cache_ttl: u64,
        #[serde(default)]
        settings: BTreeMap<String, Value>,
        #[serde(default)]
        network_allow: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum LoadedWidget {
    Link(LinkWidget),
    Lua(LuaWidget),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    pub label: String,
    #[serde(default)]
    pub description: String,
    pub url: String,
    #[serde(default)]
    pub accent: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkWidget {
    pub label: String,
    pub description: String,
    pub url: String,
    pub accent: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LuaWidget {
    pub id: String,
    #[serde(skip)]
    pub cache_ttl: u64,
    #[serde(skip)]
    pub source: String,
    #[serde(skip)]
    pub settings: BTreeMap<String, Value>,
    #[serde(skip)]
    pub network_allow: Vec<Url>,
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
    if config
        .title
        .as_deref()
        .is_some_and(|title| title.trim().is_empty())
    {
        bail!("dashboard title cannot be empty");
    }
    let Dashboard {
        title,
        subtitle,
        theme,
        accent,
        widgets,
    } = config;
    let mut script_ids = std::collections::HashSet::new();
    let mut loaded = Vec::with_capacity(widgets.len());

    for widget in widgets {
        match widget {
            WidgetConfig::Link(widget) => {
                validate_http_url(&widget.url)
                    .with_context(|| format!("invalid link URL for {}", widget.label))?;
                if widget.label.trim().is_empty() {
                    bail!("link label cannot be empty");
                }
                loaded.push(LoadedWidget::Link(LinkWidget {
                    label: widget.label,
                    description: widget.description,
                    url: widget.url,
                    accent: widget.accent,
                }));
            }
            WidgetConfig::Lua {
                id,
                script,
                cache_ttl,
                settings,
                network_allow,
            } => {
                if id.trim().is_empty() {
                    bail!("script widget id cannot be empty");
                }
                if !script_ids.insert(id.clone()) {
                    bail!("duplicate script widget id: {id}");
                }
                if !(MIN_CACHE_TTL..=MAX_CACHE_TTL).contains(&cache_ttl) {
                    bail!(
                        "widget {id} cache_ttl must be between {MIN_CACHE_TTL} and {MAX_CACHE_TTL} seconds"
                    );
                }
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
                    cache_ttl,
                    source,
                    settings,
                    network_allow,
                }));
            }
        }
    }
    Ok(Dashboard {
        title,
        subtitle,
        theme,
        accent,
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
    fn rejects_widget_columns() {
        let config = r#"
            widgets:
              - type: link
                label: Example
                url: https://example.com
                columns: 6
        "#;
        assert!(serde_yaml::from_str::<DashboardConfig>(config).is_err());
    }

    #[test]
    fn minimal_config_defers_headings_to_client_defaults() {
        let config: DashboardConfig = serde_yaml::from_str(
            r#"
                widgets:
                  - type: link
                    label: Example
                    url: https://example.com
            "#,
        )
        .unwrap();
        assert_eq!(config.title, None);
        assert_eq!(config.subtitle, None);
        assert!(matches!(config.theme, Theme::System));
        assert_eq!(config.accent, "#8b5cf6");

        let loaded = validate_and_load(config, Path::new(".")).unwrap();
        let json = serde_json::to_value(loaded).unwrap();
        assert!(json.get("title").is_none());
        assert!(json.get("subtitle").is_none());
        assert_eq!(json["widgets"][0]["type"], "link");
        assert!(json["widgets"][0].get("id").is_none());
        assert!(json["widgets"][0].get("columns").is_none());
    }

    #[test]
    fn lua_cache_ttl_defaults_to_five_minutes() {
        let config: DashboardConfig = serde_yaml::from_str(
            r#"
                widgets:
                  - type: lua
                    id: example
                    script: widget.luau
            "#,
        )
        .unwrap();
        let WidgetConfig::Lua { cache_ttl, .. } = &config.widgets[0] else {
            panic!("expected Lua widget");
        };
        assert_eq!(*cache_ttl, 300);
    }

    #[test]
    fn rejects_cache_ttls_outside_limits() {
        let config: DashboardConfig = serde_yaml::from_str(
            r#"
                widgets:
                  - type: lua
                    id: example
                    script: widgets/github-stats.luau
                    cache_ttl: 9
            "#,
        )
        .unwrap();
        assert!(validate_and_load(config, Path::new("config-example")).is_err());
    }

    #[test]
    fn rejects_unused_link_ids() {
        let config = r#"
            widgets:
              - type: link
                id: example
                label: Example
                url: https://example.com
        "#;
        assert!(serde_yaml::from_str::<DashboardConfig>(config).is_err());
    }

    #[test]
    fn rejects_nested_dashboard_settings() {
        let config = r#"
            dashboard:
              title: Test
            widgets: []
        "#;
        assert!(serde_yaml::from_str::<DashboardConfig>(config).is_err());
    }

    #[test]
    fn bundled_configuration_loads() {
        let config = load(Path::new("config-example/dashboard.yaml")).unwrap();
        assert_eq!(config.widgets.len(), 4);
        assert!(config.widgets.iter().any(
            |widget| matches!(widget, LoadedWidget::Lua(widget) if widget.id == "stelle-github")
        ));
        assert!(config.widgets.iter().any(
            |widget| matches!(widget, LoadedWidget::Link(widget) if widget.label == "YouTube")
        ));
        assert!(
            config.widgets.iter().any(
                |widget| matches!(widget, LoadedWidget::Lua(widget) if widget.id == "proxmox")
            )
        );
        assert!(config.widgets.iter().any(
            |widget| matches!(widget, LoadedWidget::Link(widget) if widget.label == "Docker Registry")
        ));
    }
}
