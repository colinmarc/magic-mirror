// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct MalformedHostPort;

impl std::fmt::Display for MalformedHostPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid host:port string")
    }
}

impl std::error::Error for MalformedHostPort {}

/// Splits a network address into the host and port components. Accepts
/// addresses of the following form:
///  - "host"
///  - "[host]"
///  - "host:port"
///  - "[host]:port"

///  # References
///
///  https://cs.opensource.google/go/go/+/refs/tags/go1.23.3:src/net/ipsock.go;l=165
pub(crate) fn split_host_port(
    hostport: impl AsRef<[u8]>,
) -> Result<(String, Option<u16>), MalformedHostPort> {
    let input = hostport.as_ref();
    let mut split = rfind(input, b':');

    let host;
    if input[0] == b'[' {
        let Some(end) = find(input, b']') else {
            return Err(MalformedHostPort);
        };

        match end + 1 {
            v if v == input.len() => {
                host = &input[1..end];
                split = None;
            }
            v if split.is_some_and(|i| v == i) => {
                host = &input[1..end];
            }
            _ => return Err(MalformedHostPort),
        }

        if find(&input[1..], b'[').is_some() || find(&input[end + 1..], b']').is_some() {
            return Err(MalformedHostPort);
        }
    } else {
        host = &input[..split.unwrap_or(input.len())];
        if find(input, b'[').is_some() || find(input, b']').is_some() {
            return Err(MalformedHostPort);
        }
    }

    let Ok(host) = std::str::from_utf8(host) else {
        return Err(MalformedHostPort);
    };

    let port = if let Some(i) = split {
        Some(
            std::str::from_utf8(&input[i + 1..])
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or(MalformedHostPort)?,
        )
    } else {
        None
    };

    Ok((host.to_owned(), port))
}

fn find(buf: &[u8], c: u8) -> Option<usize> {
    buf.iter().position(|x| x == &c)
}

fn rfind(buf: &[u8], c: u8) -> Option<usize> {
    buf.iter().rposition(|x| x == &c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_host_port() {
        macro_rules! check {
            ($s:literal, $host:literal, $port:literal) => {
                assert_eq!(Ok(($host.to_string(), Some($port))), split_host_port($s));
            };
            ($s:literal, $host:literal) => {
                assert_eq!(Ok(($host.to_string(), None)), split_host_port($s));
            };
            ($s:literal, bad) => {
                assert_eq!(Err(MalformedHostPort), split_host_port($s));
            };
        }

        check!("foo", "foo");
        check!("foo:9599", "foo", 9599);
        check!("[foo]", "foo");
        check!("[foo]:9599", "foo", 9599);
        check!("[::1]", "::1");
        check!("[::1]:9599", "::1", 9599);

        check!("foo:", bad);
        check!("foo:bar", bad);
        check!("[foo:]9599", bad);
        check!("[::1]:", bad);
        check!("[foo]]:9599", bad);
        check!("[[foo]]:9599", bad);
    }
}
