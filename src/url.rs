//! The `urlParse` and `urlUpdate` ops the shared surface's URL reads.
//!
//! The surface holds the parsed components and asks the host to parse or to
//! apply one setter; the parser is the `url` crate, so what the guest sees is
//! the WHATWG serialisation rather than this crate's own idea of one.

use ::url::Position;
use ::url::Url;
use boa_engine::Context;
use boa_engine::JsObject;
use boa_engine::JsValue;
use boa_engine::js_string;
use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;

/// Parse `input`, against `base` when given. `None` is a URL the guest should
/// see as a failure.
pub fn parse(input: &str, base: Option<&str>) -> Option<Url> {
    match base {
        Some(base) => Url::options()
            .base_url(Some(&Url::parse(base).ok()?))
            .parse(input)
            .ok(),
        None => Url::parse(input).ok(),
    }
}

/// Apply one setter to `href`. `None` leaves the URL as it was, which is what
/// the standard asks of a setter it cannot honour.
pub fn update(href: &str, part: &str, value: &str) -> Option<Url> {
    if part == "href" {
        return Url::parse(value).ok();
    }

    // Setters run against an href this module produced, so it always parses.
    let mut url = Url::parse(href).ok()?;

    match part {
        "protocol" => {
            let scheme = value.split(':').next().unwrap_or_default();
            let _ = url.set_scheme(scheme);
        }
        "username" => {
            let _ = url.set_username(value);
        }
        "password" => {
            let _ = url.set_password(Some(value));
        }
        "host" => set_host(&mut url, value),
        "hostname" => {
            // The standard's hostname setter drops a value carrying a port; the
            // url crate would silently keep the part before the colon.
            if !value.contains(':') {
                let _ = url.set_host(Some(value));
            }
        }
        "port" => set_port(&mut url, value),
        "pathname" => url.set_path(value),
        "search" => {
            let query = value.strip_prefix('?').unwrap_or(value);

            url.set_query(if query.is_empty() { None } else { Some(query) });
        }
        "hash" => {
            let fragment = value.strip_prefix('#').unwrap_or(value);

            url.set_fragment(if fragment.is_empty() {
                None
            } else {
                Some(fragment)
            });
        }
        _ => return None,
    }

    Some(url)
}

/// The host setter carries an optional port, which `Url::set_host` ignores.
fn set_host(url: &mut Url, value: &str) {
    let (host, port) = match value.rfind(':') {
        Some(colon) if !value.ends_with(']') => (&value[..colon], Some(&value[colon + 1..])),
        _ => (value, None),
    };

    if url.set_host(Some(host)).is_err() {
        return;
    }

    if let Some(port) = port {
        set_port(url, port);
    }
}

fn set_port(url: &mut Url, value: &str) {
    if value.is_empty() {
        let _ = url.set_port(None);

        return;
    }

    let Ok(port) = value.parse::<u16>() else {
        return;
    };

    let _ = url.set_port(Some(port));
}

/// The components the surface keeps as a URL's state.
pub fn components(url: &Url, context: &mut Context) -> JsObject {
    let search = match url.query() {
        Some(query) if !query.is_empty() => format!("?{query}"),
        _ => String::new(),
    };
    let hash = match url.fragment() {
        Some(fragment) if !fragment.is_empty() => format!("#{fragment}"),
        _ => String::new(),
    };
    let port = url.port().map(|port| port.to_string()).unwrap_or_default();

    let mut out = ObjectInitializer::new(context);

    for (name, value) in [
        ("href", url.as_str()),
        ("origin", &url.origin().ascii_serialization()),
        ("protocol", &format!("{}:", url.scheme())),
        ("username", url.username()),
        ("password", url.password().unwrap_or_default()),
        ("host", &url[Position::BeforeHost..Position::AfterPort]),
        ("hostname", url.host_str().unwrap_or_default()),
        ("port", &port),
        ("pathname", url.path()),
        ("search", &search),
        ("hash", &hash),
    ] {
        out.property(js_string!(name), js_string!(value), Attribute::all());
    }

    out.build()
}

/// A parsed URL as the surface wants it, or `null` when it did not parse.
pub fn to_js(url: Option<Url>, context: &mut Context) -> JsValue {
    match url {
        Some(url) => components(&url, context).into(),
        None => JsValue::null(),
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use super::update;

    #[test]
    fn a_relative_input_resolves_against_its_base() {
        let url = parse("../b", Some("http://example.com/one/two/")).unwrap();

        assert_eq!(url.as_str(), "http://example.com/one/b");
    }

    #[test]
    fn a_hostname_carrying_a_port_is_refused() {
        let url = update("http://example.com/", "hostname", "other.com:8080").unwrap();

        assert_eq!(url.host_str(), Some("example.com"));
    }

    #[test]
    fn a_host_carrying_a_port_sets_both() {
        let url = update("http://example.com/", "host", "other.com:8080").unwrap();

        assert_eq!(url.host_str(), Some("other.com"));
        assert_eq!(url.port(), Some(8080));
    }

    #[test]
    fn an_unknown_part_changes_nothing() {
        assert!(update("http://example.com/", "nonsense", "x").is_none());
    }
}
