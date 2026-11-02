//! Header facts for the Summary view and bounded readable strings from a file.
//! Nothing is decompressed or executed.

mod identify;
mod strings;

pub use identify::utc;
pub use strings::{MAX_STRINGS, strings};

/// Facts about a file from its first bytes, in display order.
/// At most the first 64 KiB are examined, even if a longer slice is supplied.
pub fn facts(probe: &[u8]) -> Vec<(String, String)> {
    let probe = &probe[..probe.len().min(64 * 1024)];
    let mut out = Vec::new();
    if let Some(id) = identify::identify(probe, crate::signature::looks_binary(probe)) {
        out.push((id.category.name().into(), id.name));
        out.extend(id.details);
    }
    let head: Vec<String> = probe.iter().take(16).map(|b| format!("{b:02X}")).collect();
    if !head.is_empty() {
        out.push(("First bytes".into(), head.join(" ")));
    }
    if probe.len() >= 256 {
        let e = entropy(probe);
        let reading = if e > 7.5 {
            "compressed or encrypted data"
        } else if e < 5.0 {
            "text or structured data"
        } else {
            "mixed content"
        };
        out.push((
            "Entropy".into(),
            format!(
                "{e:.2} bits per byte (first {} KiB): {reading}",
                probe.len() / 1024
            ),
        ));
    }
    out
}

/// Shannon entropy of the bytes, 0 to 8 bits per byte.
pub fn entropy(bytes: &[u8]) -> f64 {
    if bytes.is_empty() {
        return 0.0;
    }
    let mut counts = [0u64; 256];
    for &b in bytes {
        counts[b as usize] += 1;
    }
    let n = bytes.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = c as f64 / n;
            -p * p.log2()
        })
        .sum()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod original_tests {
    use super::*;

    #[test]
    fn entropy_bounds() {
        assert_eq!(entropy(&[]), 0.0);
        assert_eq!(entropy(&[7; 1000]), 0.0);
        let all: Vec<u8> = (0..=255).cycle().take(256 * 8).collect();
        assert!((entropy(&all) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn facts_name_the_first_bytes_and_entropy() {
        let f = facts(b"\x89PNG");
        assert_eq!(f, [("First bytes".to_owned(), "89 50 4E 47".to_owned())]);
        let text = "hello world ".repeat(100);
        let f = facts(text.as_bytes());
        assert!(f[1].1.contains("text or structured data"), "{f:?}");
    }
}
