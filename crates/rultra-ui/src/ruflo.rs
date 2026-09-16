//! The ruflo control surface, following ADR-040 from ruos-desktop.
//!
//! That app solved this problem already, and its security posture is the part
//! worth copying rather than the code:
//!
//! 1. **A fixed allowlist, keyed by id.** The client sends an id, never a
//!    command. An unknown id is a hard error, not a fallback.
//! 2. **argv, never a shell string.** Nothing the client sends is ever
//!    concatenated into a command line, so there is no metacharacter to
//!    escape and no escaping bug to have.
//! 3. **Read-only verbs only.** `status`, `list`, `stats` — nothing that
//!    spawns, runs, initialises or deletes.
//! 4. **Billing keys removed from the child's environment.** A click in a web
//!    UI must not be able to spend money, so provider keys are unset for the
//!    subprocess rather than merely unused by it.
//!
//! The one deliberate difference from ADR-040: that app runs its own server on
//! 17872 with a Host-guard, because it had no other authentication. rultra
//! already has a capability model, so this rides on it instead — introspection
//! needs [`Capability::Listen`], the same grade as reading a sensor, because
//! that is what it is.
//!
//! Commands that *do* cost money are described but never executed. The UI
//! informs; it does not silently spend.

use serde::Serialize;

/// One read-only ruflo command the UI may run.
// Serialize only: `argv` is a static slice of static strs, which serde can
// write but not read back, and nothing needs to parse a command list from the
// wire — the allowlist is compile-time by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Introspection {
    /// Stable id the client sends. Charset-restricted; see [`valid_id`].
    pub id: &'static str,
    /// Exactly the argv to execute. Fixed at compile time.
    pub argv: &'static [&'static str],
    /// What it shows, for the UI.
    pub label: &'static str,
}

/// Everything the UI is permitted to run. Grounded on ruflo's own `--help`
/// groupings, restricted to verbs that only report.
pub const READ_ONLY: &[Introspection] = &[
    Introspection {
        id: "status",
        argv: &["status"],
        label: "Stack status",
    },
    Introspection {
        id: "agents",
        argv: &["agent", "list"],
        label: "Agents",
    },
    Introspection {
        id: "swarm",
        argv: &["swarm", "status"],
        label: "Swarm",
    },
    Introspection {
        id: "hive",
        argv: &["hive-mind", "status"],
        label: "Hive-mind",
    },
    Introspection {
        id: "mcp",
        argv: &["mcp", "status"],
        label: "MCP servers",
    },
    Introspection {
        id: "memory",
        argv: &["memory", "stats"],
        label: "Memory",
    },
    Introspection {
        id: "tasks",
        argv: &["task", "list"],
        label: "Tasks",
    },
    Introspection {
        id: "sessions",
        argv: &["session", "list"],
        label: "Sessions",
    },
    Introspection {
        id: "hooks",
        argv: &["hooks", "list"],
        label: "Hooks",
    },
    Introspection {
        id: "doctor",
        argv: &["doctor"],
        label: "Doctor",
    },
];

/// Environment variables removed from the child process.
///
/// Unsetting rather than trusting the command not to use them: the guarantee
/// should not depend on which ruflo version is installed or what it does
/// internally.
pub const BILLING_ENV: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENROUTER_API_KEY",
    "GOOGLE_API_KEY",
    "AWS_SECRET_ACCESS_KEY",
];

/// Verbs that may cost money or change state. Present in the UI as text to
/// copy, never as something a click executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Costly {
    /// What it does.
    pub label: &'static str,
    /// The command to copy.
    pub command: &'static str,
    /// Why it is not a button.
    pub why: &'static str,
}

/// Shown, never run.
pub const COSTLY: &[Costly] = &[
    Costly {
        label: "Spawn an agent",
        command: "ruflo agent spawn -t coder --name my-coder",
        why: "runs a model against a provider key, which is billable",
    },
    Costly {
        label: "Initialise a swarm",
        command: "ruflo swarm init --topology hierarchical --max-agents 8",
        why: "spawns agents that call a provider",
    },
    Costly {
        label: "Store a memory with embeddings",
        command: "ruflo memory store --key k --value v",
        why: "writes to the vector store and may call an embedding model",
    },
];

/// Whether an id is even shaped like one of ours.
///
/// Checked before the lookup so a malformed id is rejected on its shape rather
/// than only by failing to match — the same belt-and-braces ADR-038 uses.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Resolve an id to its fixed argv, or nothing.
pub fn lookup(id: &str) -> Option<&'static Introspection> {
    if !valid_id(id) {
        return None;
    }
    READ_ONLY.iter().find(|c| c.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_id_resolves_to_nothing() {
        for bad in ["", "nope", "STATUS", "status;rm -rf /", "../../etc/passwd"] {
            assert!(lookup(bad).is_none(), "{bad:?} must not resolve");
        }
    }

    #[test]
    fn shell_metacharacters_cannot_even_form_a_valid_id() {
        // Belt and braces: argv execution already makes these inert, but an id
        // carrying them should be refused before it reaches a lookup.
        for bad in ["a;b", "a|b", "a&b", "a`b`", "a$b", "a b", "a/b", "a\nb"] {
            assert!(!valid_id(bad), "{bad:?} must fail the charset check");
        }
    }

    #[test]
    fn every_allowlisted_command_is_read_only() {
        // The allowlist is the security boundary, so its contents are asserted
        // rather than reviewed by eye.
        const MUTATING: &[&str] = &[
            "spawn",
            "init",
            "run",
            "execute",
            "delete",
            "remove",
            "store",
            "create",
            "terminate",
            "shutdown",
            "reset",
            "train",
            "install",
        ];
        for c in READ_ONLY {
            for verb in c.argv {
                assert!(
                    !MUTATING.contains(verb),
                    "{} runs {:?}, which is not read-only",
                    c.id,
                    c.argv
                );
            }
        }
    }

    #[test]
    fn ids_are_unique_so_a_lookup_is_unambiguous() {
        for c in READ_ONLY {
            assert_eq!(
                READ_ONLY.iter().filter(|x| x.id == c.id).count(),
                1,
                "duplicate id {}",
                c.id
            );
            assert!(valid_id(c.id), "own id {} fails the charset check", c.id);
        }
    }

    #[test]
    fn the_provider_keys_that_cost_money_are_all_unset() {
        // If a key is missing from this list, a click in a browser can bill.
        for required in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY"] {
            assert!(
                BILLING_ENV.contains(&required),
                "{required} must be removed"
            );
        }
    }

    #[test]
    fn costly_commands_are_described_but_never_in_the_runnable_set() {
        for c in COSTLY {
            assert!(
                !c.why.is_empty(),
                "{} must say why it is not a button",
                c.label
            );
            let first = c.command.split_whitespace().nth(1).unwrap_or("");
            assert!(
                !READ_ONLY.iter().any(|r| r.argv.first() == Some(&first)
                    && r.argv.len() > 1
                    && c.command.contains(r.argv[1])),
                "{} overlaps the runnable allowlist",
                c.label
            );
        }
    }

    #[test]
    fn the_allowlist_covers_the_stack_without_growing_unbounded() {
        // Small enough to audit in one screen. If this ever needs raising,
        // that is a decision, not an accident.
        assert!(
            READ_ONLY.len() <= 16,
            "allowlist has grown past auditability"
        );
        assert!(
            READ_ONLY.len() >= 6,
            "too thin to be a useful control surface"
        );
    }
}
