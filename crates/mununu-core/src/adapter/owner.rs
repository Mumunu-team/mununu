//! Who owns a gap — the closing clause of a message that reports an impossibility.
//!
//! `docs/policies/failure-paths.md` Rule 5: when there is nothing the user can do, the message
//! says so and says whose limit it is, in one of three forms. Each helper here is one form; the
//! message's head (what happened, in the input's names) is the caller's, and the helper supplies
//! only the owner clause, without a trailing period, so a caller may append a remedy of its own.
//!
//! mununu#1121 U2.10.

/// Where a defect is reported.
pub const REPORT_URL: &str = "https://github.com/Mumunu-team/mununu/issues/new";

/// **A mununu defect** — an internal invariant failed, our own lift produced malformed output, a
/// formula we emitted does not parse. Nothing in the input causes it; the ask is a report.
pub fn mununu_defect(what: &str) -> String {
    format!(
        "{what} — a mununu defect, not something the input can change: please report it, with \
         the input, at {REPORT_URL}"
    )
}

/// **A mununu limitation** — a construct, an operator, a shape the engine or the translator does
/// not carry yet; the design is fine. `instead` names what the user can do today (another
/// engine, another form, a flag that exists on the verb), when there is something.
pub fn mununu_limitation(what: &str, instead: Option<&str>) -> String {
    let mut s = format!(
        "{what} — not supported by mununu yet (a mununu limitation; nothing to change in the \
         design)"
    );
    if let Some(instead) = instead {
        s.push_str("; instead: ");
        s.push_str(instead);
    }
    s
}

/// **A limit of the technique** — explicit enumeration, a decision fragment, an abstraction pass
/// that does not converge: no knob removes the wall and it is not a budget stop. `instead` says
/// which reshaping or which other engine helps.
pub fn technique_limit(what: &str, instead: &str) -> String {
    format!("{what} — a limit of the technique, not a budget: {instead}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_defect_names_its_owner_and_asks_for_a_report_with_the_url() {
        let s = mununu_defect("the solver answered sat without a model");
        assert!(
            s.starts_with("the solver answered sat without a model — "),
            "{s}"
        );
        assert!(s.contains("a mununu defect"), "{s}");
        assert!(s.contains("please report"), "{s}");
        assert!(s.contains(REPORT_URL), "{s}");
        assert!(!s.ends_with('.'), "{s}");
    }

    #[test]
    fn a_limitation_names_its_owner_and_carries_the_alternative_only_when_given() {
        let bare = mununu_limitation("`steps` bounds are not carried", None);
        assert!(bare.contains("a mununu limitation"), "{bare}");
        assert!(bare.contains("nothing to change in the design"), "{bare}");
        assert!(!bare.contains("instead"), "{bare}");
        let with = mununu_limitation(
            "`steps` bounds are not carried",
            Some("`--engine explicit`"),
        );
        assert!(with.ends_with("; instead: `--engine explicit`"), "{with}");
        assert!(!with.ends_with('.'), "{with}");
    }

    #[test]
    fn a_technique_limit_names_its_owner_and_what_helps() {
        let s = technique_limit(
            "explicit enumeration stops at 2^20 states",
            "a symbolic engine takes the design",
        );
        assert!(s.contains("a limit of the technique"), "{s}");
        assert!(s.contains("not a budget"), "{s}");
        assert!(s.ends_with("a symbolic engine takes the design"), "{s}");
    }
}
