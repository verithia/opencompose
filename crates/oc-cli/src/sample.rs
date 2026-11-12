//! `ocp sample`: search a sample market and pull one sample, from the same
//! resolver and with the same refusal vocabulary as the agent tools.
//!
//! The command is a client of other people's catalogues: it searches one market
//! at a time and writes the file it pulls beside its licence and credit. The
//! `providers` subcommand lists the markets this build knows and why one is
//! unavailable.

use anyhow::{Result, bail};
use clap::Subcommand;
use oc_sample::http::HttpFetch;
use oc_sample::model::{Quality, SampleItem, SampleKind, SamplePage, SampleQuery};
use oc_sample::provider::SampleProvider;
use oc_sample::registry::{self, ProviderKeys};
use oc_sample::store::SampleStore;
use oc_sample::{FREESOUND_KEY_ENV, provider_for};

/// Keyed markets read their key from the environment in a headless session.
const JAMENDO_KEY_ENV: &str = "JAMENDO_CLIENT_ID";
const PIXABAY_KEY_ENV: &str = "PIXABAY_API_KEY";

/// One sample-market subcommand.
#[derive(Subcommand)]
pub enum SampleCommand {
    /// Search one sample market (keyless by default)
    Search {
        /// Words to search for, e.g. snare, vinyl loop, choir ahh
        #[arg(value_name = "QUERY")]
        query: String,

        /// Kind filter: drum, loop, vocal, fx, instrument, any
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,

        /// Include only samples at least this long
        #[arg(long, value_name = "SECONDS")]
        min_seconds: Option<f64>,

        /// Include only samples at most this long
        #[arg(long, value_name = "SECONDS")]
        max_seconds: Option<f64>,

        /// Include only this channel count, usually 1 or 2
        #[arg(long, value_name = "COUNT")]
        channels: Option<u16>,

        /// Include only results related to this tag; repeatable
        #[arg(long = "tag", value_name = "TAG")]
        tags: Vec<String>,

        /// Market id: openverse (no key) | wikimedia-commons (no key) |
        /// internet-archive (no key) | freesound (API key) | jamendo (client id)
        #[arg(long, value_name = "ID")]
        provider: Option<String>,

        /// Result offset; a page's next_offset continues the search
        #[arg(long, default_value_t = 0)]
        offset: usize,

        /// Results per page, 1..50
        #[arg(long, default_value_t = 20)]
        limit: usize,

        /// Print the result as JSON instead of a table
        #[arg(long)]
        json: bool,
    },

    /// Download one sample by id into a directory
    Pull {
        /// Sample id from `ocp sample search`, e.g. openverse:9f0c… or ia:item
        #[arg(value_name = "ID")]
        id: String,

        /// Market id; inferred from the id prefix when omitted
        #[arg(long, value_name = "ID")]
        provider: Option<String>,

        /// Destination directory
        #[arg(long, value_name = "DIR", default_value = "media/samples")]
        dest: std::path::PathBuf,

        /// Print the receipt as JSON instead of a summary
        #[arg(long)]
        json: bool,

        /// Accept a licence that is not free-to-reuse (non-commercial, no
        /// derivatives, GFDL or unknown) after reading its notice
        #[arg(long)]
        accept_licence: bool,
    },

    /// List the markets this build knows and whether each one can be searched
    Providers {
        /// Print the table as JSON
        #[arg(long)]
        json: bool,
    },
}

/// Execute one sample-market subcommand.
pub fn run(command: SampleCommand) -> Result<()> {
    match command {
        SampleCommand::Search {
            query,
            kind,
            min_seconds,
            max_seconds,
            channels,
            tags,
            provider,
            offset,
            limit,
            json,
        } => {
            let kind = match kind.as_deref() {
                Some(value) => Some(SampleKind::parse(value).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown sample kind '{value}'; accepted: drum, loop, vocal, fx, instrument, any"
                    )
                })?),
                None => None,
            };
            let query = SampleQuery {
                text: query,
                kind,
                min_seconds,
                max_seconds,
                channels,
                tags,
                offset,
                limit,
            };
            search(query, provider.as_deref(), json)
        }
        SampleCommand::Pull {
            id,
            provider,
            dest,
            json,
            accept_licence,
        } => pull(&id, provider.as_deref(), &dest, json, accept_licence),
        SampleCommand::Providers { json } => providers(json),
    }
}

/// Keys read once per call, so the borrowed view never outlives the strings.
struct Keys {
    freesound: Option<String>,
    jamendo: Option<String>,
    pixabay: Option<String>,
}

impl Keys {
    fn view(&self) -> ProviderKeys<'_> {
        ProviderKeys {
            freesound: self.freesound.as_deref(),
            jamendo: self.jamendo.as_deref(),
            pixabay: self.pixabay.as_deref(),
        }
    }
}

fn keys() -> Keys {
    Keys {
        freesound: env_key(FREESOUND_KEY_ENV),
        jamendo: env_key(JAMENDO_KEY_ENV),
        pixabay: env_key(PIXABAY_KEY_ENV),
    }
}

fn env_key(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

fn search(query: SampleQuery, provider_id: Option<&str>, json: bool) -> Result<()> {
    let provider = provider(provider_id)?;
    let fetch = HttpFetch::new().map_err(anyhow::Error::msg)?;
    let page = provider
        .search(&query, &fetch)
        .map_err(|error| anyhow::anyhow!("{}", error.message()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&page)?);
        return Ok(());
    }
    print_page(provider.label(), &page);
    Ok(())
}

fn pull(
    id: &str,
    provider_id: Option<&str>,
    dest: &std::path::Path,
    json: bool,
    accept_licence: bool,
) -> Result<()> {
    // The id prefix names the market; `--provider` only overrides it.
    let inferred = oc_sample::provider_of_id(id).map(|spec| spec.id.to_string());
    let provider = provider(provider_id.or(inferred.as_deref()))?;
    let fetch = HttpFetch::new().map_err(anyhow::Error::msg)?;
    let item = provider
        .resolve(id, &fetch)
        .map_err(|error| anyhow::anyhow!("{}", error.message()))?;
    if item.license.needs_notice() && !accept_licence {
        bail!(
            "{}\nThe file, licence and credit stay in the project; pass --accept-licence to download it.",
            item.license.notice()
        );
    }
    let store = SampleStore::new(dest);
    let receipt = store
        .pull(&item, &fetch, accept_licence)
        .map_err(|error| anyhow::anyhow!("{}", error.message()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&receipt)?);
        return Ok(());
    }
    println!("{}", receipt.path);
    println!(
        "  {} · {} · {} bytes · {}",
        receipt.kind,
        receipt.quality.as_str(),
        receipt.bytes,
        receipt.file
    );
    println!("  {}", receipt.attribution);
    if receipt.quality == Quality::PreviewMp3 {
        println!("  warning: this is a transcoded preview, not the master file");
    }
    Ok(())
}

fn providers(json: bool) -> Result<()> {
    let rows = registry::availability(keys().view());
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for row in rows {
        let state = if row.available {
            "available".to_string()
        } else {
            format!("unavailable: {}", row.reason.unwrap_or_default())
        };
        println!("{} — {} [{state}]", row.id, row.label);
        println!("  {}", row.license);
    }
    Ok(())
}

/// The licence rule, applied before a download so the refusal names the term
/// rather than arriving after a wasted request.
fn provider(id: Option<&str>) -> Result<Box<dyn SampleProvider>> {
    match provider_for(id, keys().view()) {
        Ok(provider) => Ok(provider),
        Err(error) => bail!("{}", error.message()),
    }
}

fn print_page(provider: &str, page: &SamplePage) {
    let total = match page.total {
        Some(total) => format!(" of {total}"),
        None => String::new(),
    };
    println!(
        "{provider} — {} result(s){total}, offset {}",
        page.items.len(),
        page.offset
    );
    for (index, item) in page.items.iter().enumerate() {
        let position = page.offset + index + 1;
        println!("{position}. {}", item.id);
        println!("   {}", describe(item));
        println!("   {}", item.attribution);
        if item.license.needs_notice() {
            println!(
                "   licence needs acceptance: {} (pass --accept-licence on pull)",
                item.license.name
            );
        }
    }
    match page.next_offset {
        Some(next) => println!("next page: --offset {next}"),
        None => println!("no further pages"),
    }
}

fn describe(item: &SampleItem) -> String {
    let duration = match item.duration_seconds {
        Some(seconds) => format!("{seconds:.2} s"),
        None => "duration unknown".to_string(),
    };
    let channels = match item.channels {
        Some(channels) => format!("{channels} ch"),
        None => "channels unknown".to_string(),
    };
    let rate = match item.sample_rate {
        Some(rate) => format!("{rate} Hz"),
        None => "rate unknown".to_string(),
    };
    format!(
        "{} · {duration} · {channels} · {rate} · {}",
        item.kind, item.license.name
    )
}
