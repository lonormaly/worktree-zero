//! `wt0 faq [topic]` — the plain-language questions this tool gets asked
//! most, embedded from `docs/faq.md` (the single source; the README keeps a
//! short version and links here). No project command runs anything or
//! reads a repository — this is pure static text, safe to run anywhere.

use anyhow::Result;
use clap::Args;
use serde_json::json;

/// The full FAQ, embedded at compile time from `docs/faq.md` — the single
/// source `wt0 faq` prints from and the README's short version links to.
const FAQ_MARKDOWN: &str = include_str!("../../../docs/faq.md");

#[derive(Args)]
pub struct Faq {
    /// Only show questions mentioning this word (e.g. costs, ports, safety, tilt).
    pub topic: Option<String>,
}

struct Entry<'a> {
    question: &'a str,
    answer: &'a str,
}

/// Splits `docs/faq.md`'s `## Question` sections into (question, answer)
/// pairs. The leading `# Frequently asked questions` title and its intro
/// paragraph (before the first `## `) are not a question and are skipped.
fn entries(markdown: &str) -> Vec<Entry<'_>> {
    let mut entries = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("\n## ") {
        rest = &rest[start + 4..];
        let question_end = rest.find('\n').unwrap_or(rest.len());
        let question = rest[..question_end].trim();
        let answer_start = question_end;
        let answer_end = rest[answer_start..]
            .find("\n## ")
            .map(|offset| answer_start + offset)
            .unwrap_or(rest.len());
        let answer = rest[answer_start..answer_end].trim();
        entries.push(Entry { question, answer });
        rest = &rest[answer_end..];
    }
    entries
}

/// Whether `topic` (e.g. the CLI argument "costs") is one of `question`'s
/// own words — matched by prefix in either direction so "costs" matches a
/// question containing "cost", and "safety" matches one containing "safe".
fn topic_matches(question: &str, topic: &str) -> bool {
    let topic = topic.to_lowercase();
    let stem = topic.trim_end_matches('s');
    if stem.len() < 3 {
        return question.to_lowercase().contains(&topic);
    }
    question
        .to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.len() >= 3 && (word.starts_with(stem) || stem.starts_with(word)))
}

pub fn run(args: Faq, json_output: bool) -> Result<()> {
    let all = entries(FAQ_MARKDOWN);
    let matched: Vec<&Entry> = match &args.topic {
        Some(topic) => all
            .iter()
            .filter(|entry| topic_matches(entry.question, topic))
            .collect(),
        None => all.iter().collect(),
    };

    if json_output {
        let payload = json!({
            "schema_version": 1,
            "topic": args.topic,
            "entries": matched
                .iter()
                .map(|entry| json!({ "question": entry.question, "answer": entry.answer }))
                .collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    if let Some(topic) = &args.topic {
        if matched.is_empty() {
            println!(
                "wt0 faq — no question mentions \"{topic}\". Try `wt0 faq` for the full list."
            );
            return Ok(());
        }
        println!("wt0 faq {topic}\n");
    } else {
        println!("wt0 faq — Worktree Zero questions\n");
    }

    for (index, entry) in matched.iter().enumerate() {
        if index > 0 {
            println!();
        }
        println!("▸ {}", entry.question);
        for line in entry.answer.lines() {
            if line.is_empty() {
                println!();
            } else {
                println!("  {line}");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_question_in_the_embedded_faq() {
        let parsed = entries(FAQ_MARKDOWN);
        assert!(parsed.len() >= 15, "{}", parsed.len());
        assert!(parsed
            .iter()
            .any(|entry| entry.question.starts_with("What is a worktree")));
        assert!(parsed.iter().all(|entry| !entry.answer.is_empty()));
    }

    #[test]
    fn topic_matches_costs_ports_safety_and_tilt() {
        let parsed = entries(FAQ_MARKDOWN);
        for topic in ["costs", "ports", "safety", "tilt"] {
            assert!(
                parsed
                    .iter()
                    .any(|entry| topic_matches(entry.question, topic)),
                "no question matched topic {topic:?}"
            );
        }
    }

    #[test]
    fn topic_matching_is_case_insensitive_and_word_bounded() {
        assert!(topic_matches("What does a worktree cost?", "Costs"));
        assert!(!topic_matches("What is Tilt?", "cost"));
    }
}
