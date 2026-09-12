use crate::config::{Combine, Requires, SkillSet, Windows};
use crate::model::{Now, SkillLoad};

pub struct FreshnessResult {
    pub satisfied: bool,
    pub reason: String,
}

fn latest<'a>(loads: &'a [SkillLoad], skill: &str) -> Option<&'a SkillLoad> {
    loads.iter().filter(|l| l.skill == skill).max_by_key(|l| l.at)
}

// Returns (passed, reason) for a single skill against the windows.
fn skill_fresh(skill: &str, loads: &[SkillLoad], now: &Now, w: &Windows, combine: Combine) -> (bool, String) {
    let Some(load) = latest(loads, skill) else {
        return (false, format!("skill '{skill}' never loaded this session"));
    };

    let mut checks: Vec<(bool, String)> = Vec::new();
    if w.session {
        checks.push((true, String::new())); // a load exists
    }
    if let Some(m) = w.minutes {
        let elapsed_secs = now.time.duration_since(load.at).as_secs();
        let elapsed_min = elapsed_secs.max(0) as u64 / 60;
        checks.push((elapsed_min <= m, format!("skill '{skill}' last loaded {elapsed_min}m ago (window {m}m)")));
    }
    if let Some(t) = w.turns {
        let dt = now.turn.saturating_sub(load.turn);
        checks.push((dt <= t, format!("skill '{skill}' loaded {dt} turns ago (window {t})")));
    }
    if let Some(tok) = w.tokens {
        let dtok = now.tokens.saturating_sub(load.tokens);
        checks.push((dtok <= tok, format!("skill '{skill}' loaded {dtok} tokens ago (window {tok})")));
    }

    let passed = match combine {
        Combine::All => checks.iter().all(|(ok, _)| *ok),
        Combine::Any => checks.iter().any(|(ok, _)| *ok),
    };
    let reason = if passed {
        format!("skill '{skill}' fresh")
    } else {
        checks
            .iter()
            .find(|(ok, _)| !*ok)
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| format!("skill '{skill}': no freshness window satisfied"))
    };
    (passed, reason)
}

pub fn evaluate(req: &Requires, loads: &[SkillLoad], now: &Now, combine: Combine) -> FreshnessResult {
    match &req.skills {
        SkillSet::Any(skills) => {
            let mut last_reason = "no skills configured".to_string();
            for s in skills {
                let (ok, reason) = skill_fresh(s, loads, now, &req.windows, combine);
                if ok {
                    return FreshnessResult { satisfied: true, reason };
                }
                last_reason = reason;
            }
            FreshnessResult { satisfied: false, reason: last_reason }
        }
        SkillSet::All(skills) => {
            for s in skills {
                let (ok, reason) = skill_fresh(s, loads, now, &req.windows, combine);
                if !ok {
                    return FreshnessResult { satisfied: false, reason };
                }
            }
            FreshnessResult { satisfied: true, reason: "all skills fresh".into() }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Combine, Requires, SkillSet, Windows};
    use crate::model::{Now, SkillLoad};

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    fn load(skill: &str, at: &str, turn: u64, tokens: u64) -> SkillLoad {
        SkillLoad { skill: skill.into(), at: ts(at), turn, tokens }
    }
    fn now(at: &str, turn: u64, tokens: u64) -> Now {
        Now { time: ts(at), turn, tokens }
    }

    #[test]
    fn session_satisfied_by_any_load() {
        let req = Requires { skills: SkillSet::Any(vec!["x".into()]), windows: Windows { session: true, ..Default::default() } };
        let loads = vec![load("x", "2026-09-12T10:00:00Z", 1, 100)];
        let r = evaluate(&req, &loads, &now("2026-09-12T12:00:00Z", 500, 999999), Combine::All);
        assert!(r.satisfied);
    }

    #[test]
    fn minutes_window_expires() {
        let req = Requires { skills: SkillSet::Any(vec!["x".into()]), windows: Windows { minutes: Some(15), ..Default::default() } };
        let loads = vec![load("x", "2026-09-12T10:00:00Z", 1, 100)];
        // 41 minutes later
        let r = evaluate(&req, &loads, &now("2026-09-12T10:41:00Z", 5, 200), Combine::All);
        assert!(!r.satisfied);
        assert!(r.reason.contains("x"));
    }

    #[test]
    fn all_skills_requires_each_fresh() {
        let req = Requires { skills: SkillSet::All(vec!["a".into(), "b".into()]), windows: Windows { session: true, ..Default::default() } };
        let loads = vec![load("a", "2026-09-12T10:00:00Z", 1, 100)];
        let r = evaluate(&req, &loads, &now("2026-09-12T10:05:00Z", 3, 150), Combine::All);
        assert!(!r.satisfied); // b never loaded
    }

    #[test]
    fn turns_window_and_combinator_all() {
        let req = Requires { skills: SkillSet::Any(vec!["x".into()]), windows: Windows { minutes: Some(60), turns: Some(10), ..Default::default() } };
        let loads = vec![load("x", "2026-09-12T10:00:00Z", 1, 100)];
        // within minutes(60) but 40 turns later -> AND fails
        let r = evaluate(&req, &loads, &now("2026-09-12T10:30:00Z", 41, 200), Combine::All);
        assert!(!r.satisfied);
    }

    #[test]
    fn any_combinator_reason_matches_satisfied() {
        let req = Requires {
            skills: SkillSet::Any(vec!["x".into()]),
            windows: Windows { minutes: Some(15), turns: Some(10), ..Default::default() },
        };
        let loads = vec![load("x", "2026-09-12T10:00:00Z", 1, 100)];
        // 10 minutes later (within minutes window) but 40 turns later (beyond turns window)
        let r = evaluate(&req, &loads, &now("2026-09-12T10:10:00Z", 41, 200), Combine::Any);
        assert!(r.satisfied);
        assert!(r.reason.contains("fresh"));
    }
}
