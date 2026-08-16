use std::{collections::HashSet, time::Duration};

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde::Deserialize;
use url::Url;

use crate::{
    config::{LinkWidget, TraefikWidget},
    network,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraefikRouter {
    name: String,
    rule: String,
    #[serde(default)]
    entry_points: Vec<String>,
    status: String,
    #[serde(default)]
    service: String,
    #[serde(default)]
    tls: Option<serde_json::Value>,
}

pub async fn discover(widget: &TraefikWidget) -> Result<Vec<LinkWidget>> {
    let endpoint = widget.api_url.join("api/http/routers")?;
    let routers = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .user_agent(network::USER_AGENT)
        .build()?
        .get(endpoint)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<TraefikRouter>>()
        .await
        .context("Traefik returned an invalid routers response")?;
    links_from_routers(routers, &widget.exclude_hosts)
}

fn links_from_routers(
    routers: Vec<TraefikRouter>,
    exclude_hosts: &[String],
) -> Result<Vec<LinkWidget>> {
    let excluded = exclude_hosts
        .iter()
        .map(|host| host.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut links = Vec::new();

    for router in routers {
        if router.status != "enabled" {
            continue;
        }
        let scheme = if router.tls.is_some()
            || router.entry_points.iter().any(|entrypoint| {
                matches!(
                    entrypoint.to_ascii_lowercase().as_str(),
                    "websecure" | "https"
                )
            }) {
            "https"
        } else {
            "http"
        };
        let label = if router.service == "api@internal" {
            "Traefik Dashboard".to_owned()
        } else {
            humanize_router_name(&router.name)
        };

        for host in hosts_from_rule(&router.rule) {
            let host = host.to_ascii_lowercase();
            if excluded.contains(&host) || !seen.insert(host.clone()) {
                continue;
            }
            let url = Url::parse(&format!("{scheme}://{host}"))?;
            if url.host_str() != Some(host.as_str()) {
                bail!("Traefik returned an invalid router hostname");
            }
            links.push(LinkWidget {
                label: label.clone(),
                description: String::new(),
                url,
                favicon_url: None,
                accent: None,
            });
        }
    }

    links.sort_by(|left, right| {
        left.label
            .to_ascii_lowercase()
            .cmp(&right.label.to_ascii_lowercase())
            .then_with(|| left.url.cmp(&right.url))
    });
    Ok(links)
}

fn hosts_from_rule(rule: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    let mut remaining = rule;
    while let Some(start) = remaining.find("Host(") {
        let arguments = &remaining[start + "Host(".len()..];
        let Some(end) = arguments.find(')') else {
            break;
        };
        hosts.extend(arguments[..end].split(',').filter_map(|argument| {
            let host = argument.trim().trim_matches('`').trim_matches('"').trim();
            (!host.is_empty() && !host.contains('*')).then(|| host.to_owned())
        }));
        remaining = &arguments[end + 1..];
    }
    hosts
}

fn humanize_router_name(name: &str) -> String {
    name.split('@')
        .next()
        .unwrap_or(name)
        .split(['-', '_'])
        .filter(|word| !word.is_empty())
        .map(|word| match word.to_ascii_lowercase().as_str() {
            "api" => "API".to_owned(),
            "http" => "HTTP".to_owned(),
            "https" => "HTTPS".to_owned(),
            "rsvp" => "RSVP".to_owned(),
            "ui" => "UI".to_owned(),
            "vm" => "VM".to_owned(),
            "vms" => "VMs".to_owned(),
            _ => {
                let mut characters = word.chars();
                characters
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + characters.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn router(name: &str, rule: &str, entrypoint: &str) -> TraefikRouter {
        TraefikRouter {
            name: name.into(),
            rule: rule.into(),
            entry_points: vec![entrypoint.into()],
            status: "enabled".into(),
            service: String::new(),
            tls: None,
        }
    }

    #[test]
    fn discovers_and_sorts_http_routers() {
        let links = links_from_routers(
            vec![
                router(
                    "rsvp-dashboard@docker",
                    "Host(`rsvps.example.com`)",
                    "websecure",
                ),
                router("registry-ui@docker", "Host(`registry.example.com`)", "web"),
            ],
            &[],
        )
        .unwrap();

        assert_eq!(links[0].label, "Registry UI");
        assert_eq!(links[0].url.as_str(), "http://registry.example.com/");
        assert_eq!(links[1].label, "RSVP Dashboard");
        assert_eq!(links[1].url.as_str(), "https://rsvps.example.com/");
    }

    #[test]
    fn excludes_hosts_duplicates_and_host_regexps() {
        let links = links_from_routers(
            vec![
                router("stelle@docker", "Host(`stelle.example.com`)", "websecure"),
                router(
                    "duplicate@file",
                    "Host(`stelle.example.com`, `other.example.com`)",
                    "websecure",
                ),
                router("redirect@internal", "HostRegexp(`^.+$`)", "web"),
            ],
            &["stelle.example.com".into()],
        )
        .unwrap();

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].url.as_str(), "https://other.example.com/");
    }

    #[test]
    fn names_the_internal_api_router_as_the_traefik_dashboard() {
        let mut dashboard = router(
            "dashboard@docker",
            "Host(`traefik.example.com`)",
            "websecure",
        );
        dashboard.service = "api@internal".into();

        let links = links_from_routers(vec![dashboard], &[]).unwrap();
        assert_eq!(links[0].label, "Traefik Dashboard");
    }
}
