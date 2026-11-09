use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{KitaError, Result};
use crate::version::Version;

/// A union of closed/half-open version ranges.
#[derive(Clone, Debug)]
pub struct Requirement {
    sets: Vec<Range>,
    allow_prerelease: bool,
}

#[derive(Clone, Debug)]
struct Range {
    lower: Option<(Version, bool)>,
    upper: Option<(Version, bool)>,
}

impl Range {
    fn any() -> Self {
        Self {
            lower: None,
            upper: None,
        }
    }

    fn contains(&self, version: &Version) -> bool {
        if let Some((lower, inclusive)) = &self.lower {
            match version.cmp(lower) {
                std::cmp::Ordering::Less => return false,
                std::cmp::Ordering::Equal if !inclusive => return false,
                _ => {}
            }
        }
        if let Some((upper, inclusive)) = &self.upper {
            match version.cmp(upper) {
                std::cmp::Ordering::Greater => return false,
                std::cmp::Ordering::Equal if !inclusive => return false,
                _ => {}
            }
        }
        true
    }

    fn is_empty(&self) -> bool {
        match (&self.lower, &self.upper) {
            (Some((lower, lower_inclusive)), Some((upper, upper_inclusive))) => {
                lower > upper
                    || (lower == upper && !(*lower_inclusive && *upper_inclusive))
            }
            _ => false,
        }
    }

    fn intersect_with(&self, other: &Self) -> Result<Self> {
        let lower = match (&self.lower, &other.lower) {
            (None, None) => None,
            (Some(bound), None) => Some(bound.clone()),
            (None, Some(bound)) => Some(bound.clone()),
            (Some((left, left_inclusive)), Some((right, right_inclusive))) => {
                match left.cmp(right) {
                    std::cmp::Ordering::Less => Some((right.clone(), *right_inclusive)),
                    std::cmp::Ordering::Greater => Some((left.clone(), *left_inclusive)),
                    std::cmp::Ordering::Equal => {
                        Some((left.clone(), *left_inclusive && *right_inclusive))
                    }
                }
            }
        };

        let upper = match (&self.upper, &other.upper) {
            (None, None) => None,
            (Some(bound), None) => Some(bound.clone()),
            (None, Some(bound)) => Some(bound.clone()),
            (Some((left, left_inclusive)), Some((right, right_inclusive))) => {
                match left.cmp(right) {
                    std::cmp::Ordering::Less => Some((left.clone(), *left_inclusive)),
                    std::cmp::Ordering::Greater => Some((right.clone(), *right_inclusive)),
                    std::cmp::Ordering::Equal => {
                        Some((left.clone(), *left_inclusive && *right_inclusive))
                    }
                }
            }
        };

        let range = Self { lower, upper };
        if range.is_empty() {
            return Err(KitaError::new(
                "KITA-3002",
                "版本需求内部矛盾：下界高于上界",
            ));
        }
        Ok(range)
    }
}

impl Requirement {
    pub fn parse(input: &str) -> Result<Self> {
        let input = input.trim();
        if input.is_empty() {
            return Err(invalid("版本需求不能为空"));
        }

        let mut sets = Vec::new();
        let mut allow_prerelease = false;

        for alternative in input.split("||") {
            let alternative = alternative.trim();
            if alternative.is_empty() {
                return Err(invalid("`||` 两侧不能为空"));
            }
            let mut range = Range::any();
            for token in alternative.split(',') {
                let token = token.trim();
                if token.is_empty() {
                    return Err(invalid("`,` 两侧不能为空"));
                }
                let parsed = parse_token(token)?;
                if parsed.0 {
                    allow_prerelease = true;
                }
                range = range.intersect_with(&parsed.1)?;
            }
            if range.is_empty() {
                return Err(invalid("版本需求为空区间"));
            }
            sets.push(range);
        }

        if sets.is_empty() {
            return Err(invalid("版本需求不能为空"));
        }

        Ok(Self {
            sets,
            allow_prerelease,
        })
    }

    pub fn any() -> Self {
        Self {
            sets: vec![Range::any()],
            allow_prerelease: false,
        }
    }

    pub fn contains(&self, version: &Version) -> bool {
        let matched = self.sets.iter().any(|range| range.contains(version));
        if !matched {
            return false;
        }
        if version.is_prerelease() && !self.allow_prerelease {
            return self.sets.iter().filter(|range| range.contains(version)).any(|range| {
                [&range.lower, &range.upper]
                    .into_iter()
                    .flatten()
                    .any(|(bound, _)| bound.is_prerelease() && bound.same_base(version))
            });
        }
        true
    }

    pub fn is_any(&self) -> bool {
        self.sets.len() == 1
            && self.sets[0].lower.is_none()
            && self.sets[0].upper.is_none()
    }
}

fn invalid(message: &str) -> KitaError {
    KitaError::new("KITA-3002", format!("无效版本需求：{message}"))
}

/// Returns `(allows_prerelease, range)`.
fn parse_token(token: &str) -> Result<(bool, Range)> {
    if token == "*" {
        return Ok((false, Range::any()));
    }
    if token.eq_ignore_ascii_case("latest") {
        return Ok((false, Range::any()));
    }

    let (operator, rest) = if let Some(rest) = token.strip_prefix(">=") {
        (">=", rest)
    } else if let Some(rest) = token.strip_prefix("<=") {
        ("<=", rest)
    } else if let Some(rest) = token.strip_prefix('>') {
        (">", rest)
    } else if let Some(rest) = token.strip_prefix('<') {
        ("<", rest)
    } else if let Some(rest) = token.strip_prefix('^') {
        ("^", rest)
    } else if let Some(rest) = token.strip_prefix('~') {
        ("~", rest)
    } else if let Some(rest) = token.strip_prefix('=') {
        ("=", rest)
    } else {
        ("", token)
    };

    let rest = rest.trim();
    if rest.is_empty() {
        return Err(invalid("操作符后缺少版本"));
    }

    if operator.is_empty() {
        if rest == "*" {
            return Ok((false, Range::any()));
        }
        if let Some(prefix) = rest.strip_suffix(".*") {
            let parts = numeric_parts(prefix)?;
            let caps = wildcard_upper(&parts)?;
            let lower = Version::from_parts(parts);
            let allow_pre = false;
            return Ok((
                allow_pre,
                Range {
                    lower: Some((lower, true)),
                    upper: Some((caps, false)),
                },
            ));
        }
        if rest.contains('*') {
            return Err(invalid("通配符只能出现在末尾，例如 1.2.*"));
        }
    }

    let version = Version::parse(rest)?;
    let allows_prerelease = version.is_prerelease();

    let range = match operator {
        ">=" => Range {
            lower: Some((version, true)),
            upper: None,
        },
        ">" => Range {
            lower: Some((version, false)),
            upper: None,
        },
        "<=" => Range {
            lower: None,
            upper: Some((version, true)),
        },
        "<" => Range {
            lower: None,
            upper: Some((version, false)),
        },
        "=" => Range {
            lower: Some((version.clone(), true)),
            upper: Some((version, true)),
        },
        "^" => {
            let parts = numeric_parts(rest)?;
            Range {
                lower: Some((version, true)),
                upper: Some((caret_upper(&parts)?, false)),
            }
        }
        "~" => {
            let parts = numeric_parts(rest)?;
            Range {
                lower: Some((version, true)),
                upper: Some((tilde_upper(&parts)?, false)),
            }
        }
        "" => {
            let parts = numeric_parts(rest)?;
            Range {
                lower: Some((version, true)),
                upper: Some((caret_upper(&parts)?, false)),
            }
        }
        _ => return Err(invalid("未知的版本操作符")),
    };

    Ok((allows_prerelease, range))
}

fn numeric_parts(input: &str) -> Result<Vec<u64>> {
    let base = input.split('-').next().unwrap_or(input);
    let mut parts = Vec::new();
    for part in base.split('.') {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid("版本数字段非法"));
        }
        if part.len() > 1 && part.starts_with('0') {
            return Err(invalid("数字段不允许前导零"));
        }
        let value = part
            .parse::<u64>()
            .map_err(|_| invalid("数字段超出 u64 范围"))?;
        parts.push(value);
    }
    if parts.is_empty() {
        return Err(invalid("缺少版本数字段"));
    }
    Ok(parts)
}

fn caret_upper(parts: &[u64]) -> Result<Version> {
    let major = *parts.first().ok_or_else(|| invalid("缺少主版本"))?;
    if major > 0 {
        return Ok(Version::from_parts(vec![major.saturating_add(1), 0, 0]));
    }
    match parts.len() {
        1 => Ok(Version::from_parts(vec![1, 0, 0])),
        2 => Ok(Version::from_parts(vec![0, parts[1].saturating_add(1), 0])),
        _ => {
            let minor = parts[1];
            if minor > 0 {
                Ok(Version::from_parts(vec![0, minor.saturating_add(1), 0]))
            } else {
                Ok(Version::from_parts(vec![
                    0,
                    0,
                    parts[2].saturating_add(1),
                ]))
            }
        }
    }
}

fn tilde_upper(parts: &[u64]) -> Result<Version> {
    let major = *parts.first().ok_or_else(|| invalid("缺少主版本"))?;
    match parts.get(1).copied() {
        None => Ok(Version::from_parts(vec![major.saturating_add(1), 0, 0])),
        Some(minor) => Ok(Version::from_parts(vec![
            major,
            minor.saturating_add(1),
            0,
        ])),
    }
}

fn wildcard_upper(parts: &[u64]) -> Result<Version> {
    match parts.len() {
        0 => Err(invalid("通配符缺少数字段")),
        1 => Ok(Version::from_parts(vec![parts[0].saturating_add(1), 0, 0])),
        _ => Ok(Version::from_parts(vec![
            parts[0],
            parts[1].saturating_add(1),
            0,
        ])),
    }
}

impl fmt::Display for Requirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let alternatives = self
            .sets
            .iter()
            .map(|range| {
                let mut pieces = Vec::new();
                if let Some((version, inclusive)) = &range.lower {
                    pieces.push(format!("{}{}", if *inclusive { ">=" } else { ">" }, version));
                }
                if let Some((version, inclusive)) = &range.upper {
                    pieces.push(format!("{}{}", if *inclusive { "<=" } else { "<" }, version));
                }
                if pieces.is_empty() {
                    "*".to_string()
                } else {
                    pieces.join(", ")
                }
            })
            .collect::<Vec<_>>();
        write!(f, "{}", alternatives.join(" || "))
    }
}

impl Serialize for Requirement {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Requirement {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct VisitorImpl;

        impl<'de> serde::de::Visitor<'de> for VisitorImpl {
            type Value = Requirement;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("版本需求字符串")
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<Self::Value, E> {
                Requirement::parse(value).map_err(E::custom)
            }
        }

        deserializer.deserialize_str(VisitorImpl)
    }
}

#[cfg(test)]
mod tests {
    use super::Requirement;
    use crate::version::Version;

    fn matches(requirement: &str, version: &str) -> bool {
        Requirement::parse(requirement)
            .unwrap()
            .contains(&Version::parse(version).unwrap())
    }

    #[test]
    fn caret_semantics() {
        assert!(matches("14.1", "14.1.1"));
        assert!(matches("14.1", "14.99.0"));
        assert!(!matches("14.1", "15.0.0"));
        assert!(matches("0.2.3", "0.2.9"));
        assert!(!matches("0.2.3", "0.3.0"));
        assert!(matches("0.0.3", "0.0.3"));
        assert!(!matches("0.0.3", "0.0.4"));
    }

    #[test]
    fn tilde_wildcard_and_ranges() {
        assert!(matches("~1.2", "1.2.9"));
        assert!(!matches("~1.2", "1.3.0"));
        assert!(matches("1.2.*", "1.2.7"));
        assert!(!matches("1.2.*", "1.3.0"));
        assert!(matches(">=1.2, <2", "1.9.0"));
        assert!(!matches(">=1.2, <2", "2.0.0"));
        assert!(matches("1.2 || 2.0", "2.0.4"));
    }

    #[test]
    fn prerelease_is_opt_in() {
        assert!(!matches("*", "1.0.0-beta.1"));
        assert!(matches(">=1.0.0-beta.1", "1.0.0-beta.2"));
        assert!(matches(">=1.0.0-beta.1", "1.0.0"));
    }
}
