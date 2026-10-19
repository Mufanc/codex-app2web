use axum::http::{header, HeaderMap, Uri};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

const COOKIE_NAME: &str = "codex_session";

pub(super) fn issue(headers: &HeaderMap, key: Key) -> SignedCookieJar {
    let jar = SignedCookieJar::from_headers(headers, key);
    let value = jar
        .get(COOKIE_NAME)
        .map(|cookie| cookie.value().to_owned())
        .unwrap_or_else(|| URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()));

    let cookie = Cookie::build((COOKIE_NAME, value))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .secure(!is_loopback(headers));

    jar.add(cookie)
}

pub(super) fn valid(headers: &HeaderMap, key: Key) -> bool {
    SignedCookieJar::from_headers(headers, key)
        .get(COOKIE_NAME)
        .is_some()
}

fn is_loopback(headers: &HeaderMap) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| format!("http://{value}").parse::<Uri>().ok());

    // Public hosts require HTTPS; do not trust forwarded headers to disable Secure.
    host.as_ref().and_then(Uri::host).is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "[::1]"
    })
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;

    use super::*;

    #[test]
    fn requires_a_cookie_signed_by_this_server() {
        let key = Key::generate();
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "codex.example.com".parse().unwrap());
        assert!(!valid(&headers, key.clone()));

        let response = issue(&headers, key.clone()).into_response();
        let cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
        assert!(cookie.contains("Path=/"));
        assert!(!cookie.contains("Domain="));

        let pair = cookie.split(';').next().unwrap();
        headers.insert(header::COOKIE, pair.parse().unwrap());
        assert!(valid(&headers, key.clone()));
        assert!(!valid(&headers, Key::generate()));

        headers.insert(header::COOKIE, format!("{pair}tampered").parse().unwrap());
        assert!(!valid(&headers, key));
    }

    #[test]
    fn only_loopback_hosts_allow_http() {
        for (host, secure) in [
            ("127.0.0.1:8787", false),
            ("localhost:8787", false),
            ("[::1]:8787", false),
            ("localhost.evil.example", true),
            ("codex.example.com", true),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert("x-forwarded-proto", "http".parse().unwrap());
            let response = issue(&headers, Key::generate()).into_response();
            assert_eq!(
                response.headers()[header::SET_COOKIE]
                    .to_str()
                    .unwrap()
                    .contains("Secure"),
                secure
            );
        }
    }
}
