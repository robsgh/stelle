use reqwest::StatusCode;
use url::Url;

pub const USER_AGENT: &str = concat!("stelle/", env!("CARGO_PKG_VERSION"));

pub fn is_redirect(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::MOVED_PERMANENTLY
            | StatusCode::FOUND
            | StatusCode::SEE_OTHER
            | StatusCode::TEMPORARY_REDIRECT
            | StatusCode::PERMANENT_REDIRECT
    )
}

pub fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_actionable_redirects_are_followed() {
        for status in [301, 302, 303, 307, 308] {
            assert!(is_redirect(StatusCode::from_u16(status).unwrap()));
        }
        assert!(!is_redirect(StatusCode::NOT_MODIFIED));
        assert!(!is_redirect(StatusCode::MULTIPLE_CHOICES));
    }

    #[test]
    fn origin_comparison_includes_scheme_and_port() {
        let origin = Url::parse("https://example.com:8443/page").unwrap();
        assert!(same_origin(
            &origin,
            &Url::parse("https://example.com:8443/favicon.ico").unwrap()
        ));
        assert!(!same_origin(
            &origin,
            &Url::parse("https://example.com/favicon.ico").unwrap()
        ));
    }
}
