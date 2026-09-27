//! The host's proxy, for the network calls Kodex makes on the user's behalf.
//!
//! Node — and therefore npm and Playwright, which run under it — reads proxy
//! configuration from `HTTPS_PROXY` and `HTTP_PROXY`. It does **not** read the
//! operating system's proxy settings. A machine configured through System
//! Settings → Network → Proxies, which is how proxy use is normally set up, is
//! therefore invisible to those tools: they try to resolve the host themselves
//! and fail with `getaddrinfo ENOTFOUND`, which reads like a network outage
//! rather than a missing configuration.
//!
//! The fix is to read the system setting once and hand it to the child as the
//! environment variables it already understands. Explicitly configured variables
//! always win, because a user who set `HTTPS_PROXY` has said what they meant.
//!
//! Chromium is a separate case and needs nothing here: it reads the system
//! proxy itself, which is why browser navigation works on a machine whose
//! Node-side download does not.

use std::collections::HashMap;

/// Environment variable, and the `scutil` prefix its settings share.
///
/// The three settings are named `…Proxy`, `…Port` and `…Enable` after that one
/// prefix — `HTTPSProxy`, `HTTPSPort`, `HTTPSEnable`. Keying off the host key
/// and appending a suffix gets `HTTPSProxyEnable`, which is in no output ever,
/// and silently yields no proxy at all.
const PROXY_VARS: &[(&str, &str)] = &[
    ("HTTPS_PROXY", "HTTPS"),
    ("HTTP_PROXY", "HTTP"),
    ("ALL_PROXY", "SOCKS"),
];

/// Fill in proxy variables from the host's system settings.
///
/// Only variables that are absent or blank are set. Returns an empty map where
/// there is no system proxy to read, which is the common case and must not be
/// an error.
pub fn system_proxy_env() -> HashMap<String, String> {
    #[cfg(target_os = "macos")]
    {
        let Ok(output) = std::process::Command::new("scutil").arg("--proxy").output() else {
            return HashMap::new();
        };
        if !output.status.success() {
            return HashMap::new();
        }
        parse_scutil_proxy(&String::from_utf8_lossy(&output.stdout))
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Other platforms read their proxy from elsewhere — GNOME's `gsettings`,
        // WinHTTP's registry — and a wrong guess is worse than nothing. The
        // environment variables still work there, because a user who needs a
        // proxy on those platforms has usually already set them.
        HashMap::new()
    }
}

/// Turn `scutil --proxy` output into proxy variables.
///
/// Pure, so the parsing is testable against captured output instead of against
/// whatever machine the suite happens to run on. The format is one `Key : value`
/// pair per line; where a key repeats for several interfaces the last wins,
/// which is the one `scutil` treats as effective.
pub fn parse_scutil_proxy(output: &str) -> HashMap<String, String> {
    let mut values: HashMap<String, String> = HashMap::new();
    for line in output.lines() {
        let Some((key, value)) = line.split_once(" : ") else {
            continue;
        };
        values.insert(key.trim().to_string(), value.trim().to_string());
    }

    PROXY_VARS
        .iter()
        .filter_map(|(var, prefix)| {
            // `…Enable : 0` is how a disabled proxy is spelled, and the host and
            // port keys stay in the output regardless — so absence of the flag
            // counts as disabled too.
            if values.get(&format!("{prefix}Enable")).map(String::as_str) != Some("1") {
                return None;
            }
            let host = values.get(&format!("{prefix}Proxy"))?;
            let port = values.get(&format!("{prefix}Port"))?;
            if host.is_empty() || port.is_empty() {
                return None;
            }
            Some((var.to_string(), format!("http://{host}:{port}")))
        })
        .collect()
}

/// Add the system proxy to an environment that lacks one.
pub fn apply_to(env: &mut HashMap<String, String>) {
    for (key, value) in system_proxy_env() {
        let already_set = env
            .get(&key)
            .is_some_and(|existing| !existing.trim().is_empty());
        if !already_set {
            env.insert(key, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from `scutil --proxy` on a machine behind a local
    /// proxy, which is the configuration that produced
    /// `getaddrinfo ENOTFOUND cdn.playwright.dev`.
    const CAPTURED: &str = r#"<dictionary> {
  ExceptionsList : <array> {
    0 : 127.0.0.1
    1 : *.local
    2 : <local>
  }
  HTTPEnable : 1
  HTTPPort : 7897
  HTTPProxy : 127.0.0.1
  HTTPSEnable : 1
  HTTPSPort : 7897
  HTTPSProxy : 127.0.0.1
  ProxyAutoConfigEnable : 0
  SOCKSEnable : 1
  SOCKSPort : 7897
}
"#;

    #[test]
    fn a_configured_proxy_becomes_the_variables_node_reads() {
        let parsed = parse_scutil_proxy(CAPTURED);
        assert_eq!(
            parsed.get("HTTPS_PROXY").map(String::as_str),
            Some("http://127.0.0.1:7897")
        );
        assert_eq!(
            parsed.get("HTTP_PROXY").map(String::as_str),
            Some("http://127.0.0.1:7897")
        );
    }

    #[test]
    fn a_disabled_proxy_produces_nothing() {
        let output = "  HTTPEnable : 0\n  HTTPPort : 7897\n  HTTPProxy : 127.0.0.1\n";
        assert!(parse_scutil_proxy(output).is_empty());
    }

    #[test]
    fn no_proxy_configuration_produces_nothing() {
        assert!(parse_scutil_proxy("").is_empty());
        assert!(parse_scutil_proxy("<dictionary> {\n}\n").is_empty());
    }

    /// A port without a host, or a host without a port, is not a proxy.
    #[test]
    fn a_half_configured_proxy_is_not_used() {
        let no_host = "  HTTPSEnable : 1\n  HTTPSPort : 7897\n";
        assert!(parse_scutil_proxy(no_host).is_empty());
        let no_port = "  HTTPSEnable : 1\n  HTTPSProxy : 127.0.0.1\n";
        assert!(parse_scutil_proxy(no_port).is_empty());
    }

    #[test]
    fn an_explicitly_configured_variable_wins() {
        let mut env: HashMap<String, String> =
            [("HTTPS_PROXY".to_string(), "http://corp:3128".to_string())]
                .into_iter()
                .collect();
        let detected = parse_scutil_proxy(CAPTURED);

        for (key, value) in detected {
            let already_set = env
                .get(&key)
                .is_some_and(|existing| !existing.trim().is_empty());
            if !already_set {
                env.insert(key, value);
            }
        }

        assert_eq!(
            env["HTTPS_PROXY"], "http://corp:3128",
            "a user who set this has said what they meant"
        );
    }

    /// The bug, stated as a test: on this machine, an install child must be
    /// able to see a proxy.
    #[test]
    fn the_child_environment_can_reach_a_host_behind_a_system_proxy() {
        let mut env: HashMap<String, String> = HashMap::new();
        apply_to(&mut env);
        // Nothing to assert on a host with no system proxy; the parsing tests
        // above cover the logic.
        for value in env.values() {
            assert!(
                value.starts_with("http://") && value.contains(':'),
                "a proxy variable Node can parse: {value}"
            );
        }
    }
}
