//! Single byte ranges (RFC 9110 §14). Anything this server does not serve as
//! one range, including multiple ranges, is answered with the whole file.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    Full,
    /// Inclusive byte offsets.
    Partial {
        start: u64,
        end: u64,
    },
    Unsatisfiable,
}

/// Resolve a `Range` header against a file of `len` bytes.
pub fn resolve(header: Option<&str>, len: u64) -> Range {
    let Some(spec) = header.and_then(|header| header.trim().strip_prefix("bytes=")) else {
        return Range::Full;
    };
    if spec.contains(',') {
        return Range::Full;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return Range::Full;
    };
    let number = |text: &str| -> Option<u64> {
        (!text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    match (first, last) {
        ("", suffix) => match number(suffix) {
            None => Range::Full,
            Some(0) => Range::Unsatisfiable,
            Some(_) if len == 0 => Range::Unsatisfiable,
            Some(suffix) => Range::Partial {
                start: len.saturating_sub(suffix),
                end: len - 1,
            },
        },
        (start, "") => match number(start) {
            None => Range::Full,
            Some(start) if start >= len => Range::Unsatisfiable,
            Some(start) => Range::Partial {
                start,
                end: len - 1,
            },
        },
        (start, end) => match (number(start), number(end)) {
            (Some(start), Some(end)) if start <= end => {
                if start >= len {
                    Range::Unsatisfiable
                } else {
                    Range::Partial {
                        start,
                        end: end.min(len - 1),
                    }
                }
            }
            _ => Range::Full,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{Range, resolve};

    fn partial(start: u64, end: u64) -> Range {
        Range::Partial { start, end }
    }

    #[test]
    fn single_ranges_are_clamped_to_the_file() {
        assert_eq!(resolve(Some("bytes=0-99"), 1000), partial(0, 99));
        assert_eq!(resolve(Some("bytes=100-"), 1000), partial(100, 999));
        assert_eq!(resolve(Some("bytes=-100"), 1000), partial(900, 999));
        assert_eq!(resolve(Some("bytes=-5000"), 1000), partial(0, 999));
        assert_eq!(resolve(Some("bytes=900-5000"), 1000), partial(900, 999));
        assert_eq!(resolve(Some("bytes=0-0"), 1), partial(0, 0));
        assert_eq!(resolve(Some(" bytes=5-9 "), 10), partial(5, 9));
    }

    #[test]
    fn ranges_past_the_end_are_unsatisfiable() {
        assert_eq!(resolve(Some("bytes=1000-"), 1000), Range::Unsatisfiable);
        assert_eq!(resolve(Some("bytes=1000-1001"), 1000), Range::Unsatisfiable);
        assert_eq!(resolve(Some("bytes=-0"), 1000), Range::Unsatisfiable);
        assert_eq!(resolve(Some("bytes=0-"), 0), Range::Unsatisfiable);
        assert_eq!(resolve(Some("bytes=-1"), 0), Range::Unsatisfiable);
    }

    #[test]
    fn anything_else_serves_the_whole_file() {
        for header in [
            None,
            Some(""),
            Some("bytes=5-2"),
            Some("bytes=0-1,3-4"),
            Some("items=0-9"),
            Some("bytes=a-9"),
            Some("bytes=+1-9"),
            Some("bytes=-"),
            Some("bytes=10"),
        ] {
            assert_eq!(resolve(header, 1000), Range::Full, "{header:?}");
        }
    }
}
