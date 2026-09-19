use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "jevi",
    version,
    about = "Ask typed questions about a text and branch on the answer.",
    long_about = "Ask typed questions about a text and branch on the answer.\n\n\
        Exit codes: 0 yes, 1 no, 3 unsure, 4 no answer (no key, no network, API error),\n\
        5 invalid input. Never 2 — a hook's exit 2 means \"block\" to Claude Code.\n\n\
        With --soft, 4 and 5 become 0 and stdout carries {\"ok\":false,...}: branch on the\n\
        JSON, not on the exit code.",
    disable_help_subcommand = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

// `Ask` is much bigger than the other variants, and this enum is built exactly once per
// process from the command line. Boxing it would buy nothing and cost a deref everywhere.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Ask one or more questions about a text.
    Ask(Ask),
    /// Show where the config, key and question sets are resolved from.
    Doctor {
        /// Make one real request, and print its latency and cost.
        #[arg(long)]
        live: bool,
    },
    /// Manage the stored configuration.
    #[command(subcommand)]
    Config(ConfigCmd),
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Read a key from stdin and store it at mode 0600 (keeps it out of shell history).
    SetKey {
        /// openrouter or typesafe
        provider: String,
    },
}

#[derive(Args, Debug)]
pub struct Ask {
    /// The question, for a one-off from a terminal. Use -f for anything you run twice.
    pub instructions: Option<String>,

    /// A question set: a path, or a name looked up in ./.jevi/ then the config directory.
    #[arg(short = 'f', long = "questions")]
    pub set: Option<String>,

    /// Turn the positional question into a choice between these options.
    #[arg(long, value_name = "a,b,c", conflicts_with = "levels")]
    pub options: Option<String>,

    /// Turn the positional question into a score over these levels, lowest first.
    #[arg(long, value_name = "low,mid,high")]
    pub levels: Option<String>,

    /// Read the text to judge from this file instead of stdin.
    #[arg(long, value_name = "PATH", conflicts_with = "state_text")]
    pub state: Option<String>,

    /// The text to judge, inline.
    #[arg(short = 's', long = "text", value_name = "TEXT")]
    pub state_text: Option<String>,

    /// Parse the text as JSON and send it as an object or array, not as a string.
    #[arg(long)]
    pub state_json: bool,

    /// Print the normalised result document.
    #[arg(long)]
    pub json: bool,

    /// Print the provider's own response, untouched.
    #[arg(long)]
    pub raw: bool,

    /// Never fail loudly: exit 0 even with no key, no network or an API error.
    #[arg(long)]
    pub soft: bool,

    /// Print latency and usage to stderr.
    #[arg(short, long)]
    pub verbose: bool,

    /// yes at or above this probability (positional question only).
    #[arg(long, value_name = "P")]
    pub yes_at: Option<f64>,

    /// no at or below this probability (positional question only).
    #[arg(long, value_name = "P")]
    pub no_at: Option<f64>,

    /// Minimum confidence for a choice or score (positional question only).
    #[arg(long, value_name = "P")]
    pub min_confidence: Option<f64>,

    /// Truncate a string state to this many characters. 0, the default, sends it whole.
    #[arg(long, value_name = "N")]
    pub max_chars: Option<usize>,

    /// Total budget in milliseconds, retry included.
    #[arg(long, value_name = "MS")]
    pub timeout: Option<u64>,

    /// openrouter or typesafe. Defaults to whichever has a key.
    #[arg(long)]
    pub provider: Option<String>,

    /// Override the model id.
    #[arg(long)]
    pub model: Option<String>,
}
