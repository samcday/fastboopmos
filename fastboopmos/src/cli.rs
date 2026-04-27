use clap::{Parser, Subcommand};
use std::path::PathBuf;

const DEFAULT_INDEX_URL: &str = "https://images.postmarketos.org/bpo/index.json";
const DEFAULT_CACHE_URL: &str =
    "https://s3.eu-central-003.backblazeb2.com/samcday-fastboopmos/fastboopmos";

#[derive(Debug, Parser)]
#[command(
    name = "fastboopmos",
    about = "Build edge.channel from pmOS index + device templates, using local artifact/bootpro caches."
)]
pub struct Cli {
    #[arg(long, default_value = ".", global = true)]
    pub templates_dir: PathBuf,

    #[arg(long, default_value = DEFAULT_INDEX_URL, global = true)]
    pub index_url: String,

    #[arg(long, default_value = "edge", global = true)]
    pub release: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Emit a JSON array of `{device, ui}` entries suitable for a GHA matrix.
    List(ListArgs),
    /// Compile bootpros for a single device into the bootpro cache.
    Build(BuildArgs),
    /// Assemble the indexed channel from cached bootpros (no compile).
    Channel(ChannelArgs),
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Restrict matrix to a single device id.
    #[arg(long)]
    pub device: Option<String>,

    /// Restrict matrix to a single UI (phosh, gnome-mobile, ...).
    #[arg(long)]
    pub ui: Option<String>,
}

#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// pmOS device id (template filename stem).
    #[arg(long)]
    pub device: String,

    /// Limit selected rootfs images to a single UI.
    #[arg(long)]
    pub ui: Option<String>,

    #[arg(long, default_value = "build/pmos-artifacts")]
    pub artifact_cache_dir: PathBuf,

    #[arg(long, default_value = "build/pmos-bootpros")]
    pub bootpro_cache_dir: PathBuf,

    /// Base URL to fetch pre-built `.bootpro` cache entries over HTTP before
    /// compiling. Pass an empty string to disable the HTTP cache.
    #[arg(long, env = "FASTBOOPMOS_CACHE_URL", default_value = DEFAULT_CACHE_URL)]
    pub cache_url: String,
}

#[derive(Debug, clap::Args)]
pub struct ChannelArgs {
    /// Restrict assembly to a single device id (manual partial-channel use only).
    #[arg(long)]
    pub device: Option<String>,

    /// Restrict assembly to a single UI (manual partial-channel use only).
    #[arg(long)]
    pub ui: Option<String>,

    #[arg(long, default_value = "build/pmos-bootpros")]
    pub bootpro_cache_dir: PathBuf,

    #[arg(long, default_value = "dist/edge.channel")]
    pub output: PathBuf,

    /// Base URL to fetch pre-built `.bootpro` cache entries over HTTP. Pass
    /// an empty string to disable the HTTP cache (channel will then succeed
    /// only if every bootpro is present in the local cache dir).
    #[arg(long, env = "FASTBOOPMOS_CACHE_URL", default_value = DEFAULT_CACHE_URL)]
    pub cache_url: String,
}
