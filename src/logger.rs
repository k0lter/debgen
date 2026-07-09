use gilt::color::ColorSystem;
use gilt::console::Console;
use tracing::level_filters::LevelFilter;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, Writer};
use tracing_subscriber::fmt::{self, FmtContext};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;

/// Semantic markup tags and their gilt inline style equivalents.
/// Gilt does not resolve custom theme names in markup yet, so we expand them here.
const SEMANTIC_TAGS: &[(&str, &str)] = &[
    ("pkg", "bold #14b8a6"),
    ("path", "underline #bcbcf5"),
    ("version", "bold #06b6d4"),
    ("url", "underline #0ea5e9"),
    ("cmd", "bold #f59e0b"),
    ("field", "#94a3b8"),
    ("value", "bold #22c55e"),
    ("ok", "bold #10b981"),
    ("skip", "dim #9ca3af"),
    ("action", "bold #ffa1b0"),
];

fn console() -> Console {
    Console::builder()
        .force_terminal(true)
        .no_color(false)
        .color_system_override(ColorSystem::TrueColor)
        .markup(true)
        .width(200)
        .build()
}

fn with_console<F, R>(f: F) -> R
where
    F: FnOnce(&Console) -> R,
{
    let console = console();
    f(&console)
}

fn expand_semantic_tags(input: &str) -> String {
    let mut result = input.to_string();
    for (name, style) in SEMANTIC_TAGS {
        result = result.replace(&format!("[{name}]"), &format!("[{style}]"));
    }
    result
}

pub fn render_markup(input: &str) -> String {
    let expanded = expand_semantic_tags(input);
    with_console(|console| {
        let text = console.render_str(&expanded, None, None, None);
        let segments = console.render(&text, None);
        console.render_buffer(&segments).trim_end().to_string()
    })
}

struct GiltFormatter;

impl<S, N> FormatEvent<S, N> for GiltFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> std::fmt::Result {
        let level = *event.metadata().level();

        let level_tag = match level {
            Level::ERROR => render_markup("[bold #e5484d]error[/]"),
            Level::WARN => render_markup("[bold #f76b15]warn[/]"),
            Level::INFO => render_markup("[bold #2f81f7]info[/]"),
            Level::DEBUG => render_markup("[bold #8b5cf6]debug[/]"),
            Level::TRACE => render_markup("[dim #64748b]trace[/]"),
        };

        let mut buf = String::new();
        ctx.field_format()
            .format_fields(Writer::new(&mut buf), event)?;

        let styled_msg = render_markup(&buf);

        writeln!(writer, "{level_tag} {styled_msg}")
    }
}

pub fn init(verbosity: u8, quiet: bool) {
    let filter = if quiet {
        LevelFilter::ERROR
    } else {
        match verbosity {
            0 => LevelFilter::WARN,
            1 => LevelFilter::INFO,
            2 => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        }
    };

    let debgen_directive = format!(
        "debgen={}",
        match filter {
            LevelFilter::ERROR => "error",
            LevelFilter::WARN => "warn",
            LevelFilter::INFO => "info",
            LevelFilter::DEBUG => "debug",
            LevelFilter::TRACE => "trace",
            LevelFilter::OFF => "off",
        }
    );

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::OFF.into())
        .from_env_lossy()
        .add_directive(debgen_directive.parse().expect("valid filter directive"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().event_format(GiltFormatter))
        .init();
}

pub fn enabled_info() -> bool {
    tracing::enabled!(tracing::Level::INFO)
}

/// Alias for `bail!` — messages may contain semantic markup tags, rendered at display time.
#[macro_export]
macro_rules! error_msg {
    ($msg:expr) => {
        anyhow::bail!($msg)
    };
    ($fmt:expr, $($arg:tt)*) => {
        anyhow::bail!($fmt, $($arg)*)
    };
}
