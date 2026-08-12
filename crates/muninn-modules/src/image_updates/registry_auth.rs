//! Credentials for registries that require authentication, and the one header
//! that carries them.
//!
//! # Why this exists at all
//!
//! ADR-0013 chose to ask the Docker daemon rather than talk to registries
//! directly, and recorded that muninn therefore needs no credential handling:
//! the daemon "already knows any registry credentials the host is configured
//! with". That sentence was wrong, and nothing executed it until cell I11 of
//! `scripts/image-updates-test.sh` did.
//!
//! `docker login` does not tell the daemon anything. It authenticates against
//! the registry and writes the result into the **client's**
//! `~/.docker/config.json`; the Docker CLI then reads that file and forwards
//! the credentials to the daemon in an `X-Registry-Auth` header on every
//! request that needs them. The daemon keeps none of its own — daemon-side
//! credential storage has been asked for upstream twice (moby/moby#41706,
//! moby/moby#11820) and implemented neither time.
//!
//! So the socket gives muninn the ability to *ask*, not the credentials to ask
//! *with*. muninn replaces the CLI here rather than using it, and has to send
//! that header itself.
//!
//! # The encoding is not negotiable
//!
//! The value is **base64url** (RFC 4648 §5, the `-_` alphabet), not standard
//! base64. The daemon decodes with Go's `base64.URLEncoding`, which rejects
//! `+` and `/`, and the API documentation has never said so — that is
//! moby/moby#33434, open since 2017. A standard-base64 encoder produces a
//! header that works until a credential happens to encode a `+`, and then
//! fails for reasons nothing explains.

use muninn_core::config::model::RegistryAuth;
use muninn_core::secret::Secret;

/// One registry's credentials, with the password already resolved from its
/// file.
///
/// Resolution happens once, in the binary, so this type never holds a path and
/// the read never happens per container.
#[derive(Debug, Clone)]
pub struct RegistryCredential {
    /// The registry host as it appears in a *normalised* image reference:
    /// `docker.io`, `registry.example.com`, `registry.example.com:5000`.
    pub registry: String,
    pub username: String,
    pub password: Secret,
}

/// Read each configured password from the file the configuration names.
///
/// Returns what it could resolve, and one message per entry it could not. The
/// caller decides what to do with those — both callers print them and carry
/// on, because an unreadable credential must fail the containers on that
/// registry rather than the whole check. Nothing here reports a healthy value,
/// which is the invariant that matters.
///
/// A failure names the registry and the path, never the contents.
pub fn resolve(entries: &[RegistryAuth]) -> (Vec<RegistryCredential>, Vec<String>) {
    let mut credentials = Vec::with_capacity(entries.len());
    let mut problems = Vec::new();

    for entry in entries {
        match Secret::from_file(&entry.password_file) {
            Ok(password) => credentials.push(RegistryCredential {
                registry: entry.registry.clone(),
                username: entry.username.clone(),
                password,
            }),
            Err(e) => problems.push(format!(
                "no credential for '{}' — {}: {e}",
                entry.registry, entry.password_file
            )),
        }
    }
    (credentials, problems)
}

/// The registry host of a normalised reference.
///
/// Every reference reaching this point has been through
/// `check::normalize_repository`, which guarantees a leading registry host —
/// so this is the first path component and there is no Docker Hub special case
/// left to handle here.
fn registry_of(reference: &str) -> &str {
    reference.split('/').next().unwrap_or(reference)
}

/// The `X-Registry-Auth` value for a reference, or `None` when no credential is
/// configured for its registry.
///
/// `None` means "send no header", which is exactly right for a public
/// registry: an anonymous distribution query succeeds there, and sending an
/// empty credential would turn a working anonymous lookup into a rejected
/// authenticated one.
pub fn header_for(credentials: &[RegistryCredential], reference: &str) -> Option<String> {
    let host = registry_of(reference);
    let cred = credentials.iter().find(|c| c.registry == host)?;
    Some(encode_auth(cred))
}

/// The header value: base64url of the daemon's `AuthConfig` JSON.
///
/// Serialised with `serde_json` rather than `format!`, because the password is
/// arbitrary bytes and hand-built JSON is how a backslash or a quote in a
/// credential becomes a malformed request the daemon rejects without saying
/// why (moby/moby#44064).
fn encode_auth(cred: &RegistryCredential) -> String {
    #[derive(serde::Serialize)]
    struct AuthConfig<'a> {
        username: &'a str,
        password: &'a str,
        serveraddress: &'a str,
    }

    // `expose()` is called here and nowhere else in this module: the value goes
    // straight into the encoder and the encoded form goes straight onto the
    // wire. It is never logged, never put in an error and never in argv.
    let json = serde_json::to_vec(&AuthConfig {
        username: &cred.username,
        password: cred.password.expose(),
        serveraddress: &cred.registry,
    })
    // A struct of three strings cannot fail to serialise; the fallback is an
    // empty credential rather than a panic, and an empty one is refused by the
    // registry loudly.
    .unwrap_or_default();

    base64url(&json)
}

/// base64url (RFC 4648 §5) **with** padding.
///
/// Padded because the daemon decodes with Go's `base64.URLEncoding`, which
/// expects it; `RawURLEncoding` — the unpadded variant — is a different
/// decoder and is not the one on the other end.
///
/// Hand-rolled rather than adding a crate: this is a 64-entry table and three
/// shifts, it encodes only (no untrusted input is ever decoded here), and a new
/// dependency needs approval and widens the supply chain for something the
/// tests below pin to the RFC's own vectors.
fn base64url(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);

        out.push(ALPHABET[(n >> 18 & 0x3F) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(registry: &str, user: &str, pass: &str) -> RegistryCredential {
        RegistryCredential {
            registry: registry.to_string(),
            username: user.to_string(),
            password: Secret::from_value(pass),
        }
    }

    // RFC 4648 §10's own vectors, which is what makes this a check rather than
    // a record of what the function happens to do.
    #[test]
    fn encodes_the_rfc_4648_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg==");
        assert_eq!(base64url(b"fo"), "Zm8=");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg==");
        assert_eq!(base64url(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
    }

    /// The whole reason this is not `base64::encode`: bytes 0xFB 0xFF encode to
    /// `+/` in the standard alphabet and `-_` in this one, and the daemon's
    /// decoder rejects the former.
    #[test]
    fn uses_the_url_safe_alphabet_not_the_standard_one() {
        let encoded = base64url(&[0xFB, 0xFF, 0xBF]);
        assert!(
            !encoded.contains('+') && !encoded.contains('/'),
            "must not emit the standard alphabet's + or /: {encoded}"
        );
        assert_eq!(encoded, "-_-_");
    }

    #[test]
    fn a_reference_with_no_configured_registry_sends_no_header() {
        let creds = vec![cred("registry.example.com", "robot", "pw")];
        assert!(header_for(&creds, "docker.io/library/alpine:3.19").is_none());
    }

    #[test]
    fn the_matching_registry_is_selected_by_host() {
        let creds = vec![
            cred("other.example.com", "wrong", "wrong"),
            cred("registry.example.com", "robot", "pw"),
        ];
        let header = header_for(&creds, "registry.example.com/team/app:v1").expect("a header");
        // Decoding is not this module's job, so the assertion is on the shape
        // the daemon will decode rather than on an opaque string.
        assert_eq!(
            header,
            base64url(
                br#"{"username":"robot","password":"pw","serveraddress":"registry.example.com"}"#
            )
        );
    }

    #[test]
    fn a_port_in_the_registry_host_is_part_of_the_match() {
        let creds = vec![cred("registry.example.com:5000", "robot", "pw")];
        assert!(header_for(&creds, "registry.example.com:5000/team/app:v1").is_some());
        // Same host, no port — a different registry as far as a reference is
        // concerned, and matching it would send credentials somewhere they were
        // not configured for.
        assert!(header_for(&creds, "registry.example.com/team/app:v1").is_none());
    }

    #[test]
    fn docker_hub_can_be_authenticated_too() {
        let creds = vec![cred("docker.io", "robot", "pw")];
        assert!(header_for(&creds, "docker.io/library/alpine:3.19").is_some());
    }

    /// A quote or a backslash in a password is what breaks a hand-built JSON
    /// body (moby/moby#44064), so the encoder has to be a real serialiser.
    #[test]
    fn a_password_with_json_metacharacters_is_escaped() {
        let creds = vec![cred("r.example.com", "u", "pa\"ss\\word")];
        let header = header_for(&creds, "r.example.com/app:v1").expect("a header");
        assert_eq!(
            header,
            base64url(
                br#"{"username":"u","password":"pa\"ss\\word","serveraddress":"r.example.com"}"#
            )
        );
    }

    /// The encoded form is what goes into a request line built by hand, so it
    /// must not be able to carry a header separator into it.
    #[test]
    fn the_encoded_header_never_contains_control_characters() {
        let creds = vec![cred("r.example.com", "u\r\nX-Evil: 1", "p\r\n")];
        let header = header_for(&creds, "r.example.com/app:v1").expect("a header");
        assert!(
            !header.chars().any(|c| c.is_control()),
            "base64url output must be free of control characters: {header}"
        );
    }
}
