use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "fastboopmos",
    about = "Build edge.channel from pmOS index + device templates, using local artifact/bootpro caches."
)]
pub struct Args {
    #[arg(long, default_value = ".")]
    pub templates_dir: PathBuf,

    #[arg(long)]
    pub only_device: Option<String>,

    #[arg(long, default_value = "https://images.postmarketos.org/bpo/index.json")]
    pub index_url: String,

    #[arg(long, default_value = "edge")]
    pub release: String,

    #[arg(long, default_value = "fastboop")]
    pub fastboop: PathBuf,

    #[arg(long, default_value = "build/pmos-artifacts")]
    pub artifact_cache_dir: PathBuf,

    #[arg(long, default_value = "build/pmos-bootpros")]
    pub bootpro_cache_dir: PathBuf,

    #[arg(long, default_value = "dist/edge.channel")]
    pub output: PathBuf,
}
