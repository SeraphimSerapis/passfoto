use std::fs::File;
use std::io::{self, Read};

/// A 128-bit session secret, hex-encoded. Lives only in process memory.
pub struct Token(String);

impl Token {
    pub fn generate() -> io::Result<Self> {
        let mut bytes = [0u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        Ok(Self(bytes.iter().map(|b| format!("{b:02x}")).collect()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn matches(&self, presented: &str) -> bool {
        ct_eq(self.0.as_bytes(), presented.as_bytes())
    }
}

/// Which secret a request proved it holds. The two are deliberately disjoint:
/// the agent token is throttled, the preview token only reaches the human's viewfinder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Agent,
    Preview,
}

pub struct Tokens {
    pub agent: Token,
    pub preview: Token,
}

impl Tokens {
    pub fn generate() -> io::Result<Self> {
        Ok(Self {
            agent: Token::generate()?,
            preview: Token::generate()?,
        })
    }

    pub fn role_of(&self, presented: &str) -> Option<Role> {
        // Compare against both unconditionally so timing does not reveal which one was close.
        let agent = self.agent.matches(presented);
        let preview = self.preview.matches(presented);
        match (agent, preview) {
            (true, _) => Some(Role::Agent),
            (false, true) => Some(Role::Preview),
            (false, false) => None,
        }
    }
}

/// Constant time in the content of the inputs. Length is not secret: every token is 32 chars.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let diff = a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    std::hint::black_box(diff) == 0
}

/// The token a request presents: `Authorization: Bearer <t>` wins over `?token=<t>`,
/// which stays supported for browsers and older agents.
pub fn presented_token<'a>(authorization: Option<&'a str>, url: &'a str) -> Option<&'a str> {
    if let Some(value) = authorization {
        let (scheme, rest) = value.trim().split_once(' ')?;
        return scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim());
    }
    query_param(url, "token")
}

pub fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    url.split_once('?')?
        .1
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(s: &str) -> Token {
        Token(s.to_string())
    }

    #[test]
    fn generated_token_is_32_hex_chars() {
        let t = Token::generate().unwrap();
        assert_eq!(t.as_str().len(), 32);
        assert!(t.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn generated_tokens_differ() {
        let t = Tokens::generate().unwrap();
        assert_ne!(t.agent.as_str(), t.preview.as_str());
    }

    #[test]
    fn ct_eq_requires_exact_match() {
        assert!(ct_eq(b"abcd", b"abcd"));
        assert!(!ct_eq(b"abcd", b"abce"));
        assert!(!ct_eq(b"abcd", b"abc"));
        assert!(!ct_eq(b"", b"a"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn role_of_separates_agent_and_preview() {
        let t = Tokens {
            agent: token("aaaa"),
            preview: token("pppp"),
        };
        assert_eq!(t.role_of("aaaa"), Some(Role::Agent));
        assert_eq!(t.role_of("pppp"), Some(Role::Preview));
        assert_eq!(t.role_of("aaa"), None);
        assert_eq!(t.role_of(""), None);
    }

    #[test]
    fn bearer_header_is_preferred_over_query() {
        assert_eq!(
            presented_token(Some("Bearer abc"), "/snapshot?token=xyz"),
            Some("abc")
        );
        assert_eq!(
            presented_token(Some("bearer  abc "), "/snapshot"),
            Some("abc")
        );
        assert_eq!(
            presented_token(Some("BEARER abc"), "/snapshot"),
            Some("abc")
        );
    }

    #[test]
    fn non_bearer_header_presents_nothing() {
        // A wrong scheme must not silently fall back to the query string.
        assert_eq!(
            presented_token(Some("Basic abc"), "/snapshot?token=xyz"),
            None
        );
        assert_eq!(presented_token(Some("Bearer"), "/snapshot?token=xyz"), None);
    }

    #[test]
    fn query_token_still_works() {
        assert_eq!(
            presented_token(None, "/snapshot?token=abc&width=640"),
            Some("abc")
        );
        assert_eq!(presented_token(None, "/snapshot?width=640"), None);
        assert_eq!(presented_token(None, "/snapshot"), None);
    }

    #[test]
    fn query_param_matches_whole_keys() {
        assert_eq!(query_param("/snapshot?token=abc&x=1", "token"), Some("abc"));
        assert_eq!(query_param("/snapshot?x=1&token=abc", "token"), Some("abc"));
        assert_eq!(query_param("/snapshot?mytoken=abc", "token"), None);
        assert_eq!(query_param("/snapshot", "token"), None);
    }
}
